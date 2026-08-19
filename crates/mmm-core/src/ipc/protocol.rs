//! Wire messages exchanged between the host and `mmm-ipc-worker` over stdin/stdout.
//!
//! One frame is `[u8 tag][u32 LE payload_len][payload bytes]`. Band-carrying
//! messages ([`BandRequest`], [`BandReply`], [`OutputBand`]) encode as a
//! fixed little-endian binary payload (cheap to build, no allocation-heavy
//! parsing on the hot per-band path); every other message encodes as JSON
//! ([`serde_json`]). See [`write_frame`] / [`read_worker_frame`] /
//! [`read_host_frame`] for the codec, and [`FrameBody::encode`] for the
//! tag assignment.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::blend::{BlendMode, BlendParams};
use crate::formats::{PropertyValue, XisfProperty};
use crate::photometry::GainMode;

/// Worker→host: asks the host to fill a shared-memory slot with rows
/// `[y0, y1)` of panel `panel_id`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct BandRequest {
    /// Correlates this request with its [`BandReply`].
    pub request_id: u32,
    /// Panel to read from ([`PanelDesc::panel_id`]).
    pub panel_id: u32,
    /// First row requested (inclusive).
    pub y0: u64,
    /// Last row requested (exclusive).
    pub y1: u64,
    /// Shared-memory input slot the host should fill.
    pub slot_id: u32,
}

impl BandRequest {
    const WIRE_LEN: usize = 4 + 4 + 8 + 8 + 4;

    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_LEN);
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        buf.extend_from_slice(&self.panel_id.to_le_bytes());
        buf.extend_from_slice(&self.y0.to_le_bytes());
        buf.extend_from_slice(&self.y1.to_le_bytes());
        buf.extend_from_slice(&self.slot_id.to_le_bytes());
        buf
    }

    fn from_bytes(buf: &[u8]) -> io::Result<Self> {
        expect_len("BandRequest", Self::WIRE_LEN, buf)?;
        Ok(Self {
            request_id: read_u32(buf, 0),
            panel_id: read_u32(buf, 4),
            y0: read_u64(buf, 8),
            y1: read_u64(buf, 16),
            slot_id: read_u32(buf, 24),
        })
    }
}

/// Host→worker: the requested band is ready in `slot_id` (or an error
/// occurred while filling it).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct BandReply {
    /// The [`BandRequest::request_id`] this replies to.
    pub request_id: u32,
    /// The slot the band was written to (or would have been).
    pub slot_id: u32,
    /// `0` = ok, `1` = error.
    pub status: u8,
}

impl BandReply {
    const WIRE_LEN: usize = 4 + 4 + 1;

    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_LEN);
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        buf.extend_from_slice(&self.slot_id.to_le_bytes());
        buf.push(self.status);
        buf
    }

    fn from_bytes(buf: &[u8]) -> io::Result<Self> {
        expect_len("BandReply", Self::WIRE_LEN, buf)?;
        Ok(Self {
            request_id: read_u32(buf, 0),
            slot_id: read_u32(buf, 4),
            status: buf[8],
        })
    }
}

/// Worker→host: a blended output band is ready in `slot_id`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct OutputBand {
    /// Correlates this band with the host's [`HostMsg::OutputAck`].
    pub request_id: u32,
    /// First output row in this band.
    pub y0: u64,
    /// Row count in this band.
    pub rows: u64,
    /// Shared-memory output slot holding the pixel data.
    pub slot_id: u32,
}

impl OutputBand {
    const WIRE_LEN: usize = 4 + 8 + 8 + 4;

    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::WIRE_LEN);
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        buf.extend_from_slice(&self.y0.to_le_bytes());
        buf.extend_from_slice(&self.rows.to_le_bytes());
        buf.extend_from_slice(&self.slot_id.to_le_bytes());
        buf
    }

    fn from_bytes(buf: &[u8]) -> io::Result<Self> {
        expect_len("OutputBand", Self::WIRE_LEN, buf)?;
        Ok(Self {
            request_id: read_u32(buf, 0),
            y0: read_u64(buf, 4),
            rows: read_u64(buf, 12),
            slot_id: read_u32(buf, 20),
        })
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap())
}

fn read_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
}

fn expect_len(what: &str, expected: usize, buf: &[u8]) -> io::Result<()> {
    if buf.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{what} frame: expected {expected} bytes, got {}", buf.len()),
        ));
    }
    Ok(())
}

/// How the worker should read/align input panels for a run.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum JobMode {
    /// Panels are already registered to a common canvas (MosaicByCoordinates
    /// output); pixels are read as-is.
    Aligned,
    /// Panels carry a plate solution (in [`PanelDesc::properties`]) and must
    /// be reprojected onto the shared canvas before blending.
    Solved,
    /// Panels are read directly from these file paths instead of over the
    /// shared-memory band protocol.
    Files {
        /// One path per panel, in `panels` order.
        paths: Vec<String>,
        /// The `Auto/Aligned/Solved` override (default `Auto`).
        #[serde(default)]
        input_select: InputSelectWire,
    },
}

/// Wire form of [`crate::analyze::InputSelect`] — the UI's `Auto/Aligned/Solved`
/// override, threaded to the worker only for [`JobMode::Files`] (views modes are
/// resolved host-side into `Aligned`/`Solved` directly). Serializes as a bare
/// JSON string via serde's externally-tagged unit-variant encoding.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InputSelectWire {
    /// Detect aligned-vs-solved from the files (the default).
    #[default]
    Auto,
    /// Force the aligned full-canvas path.
    Aligned,
    /// Force the solved (reproject) path.
    Solved,
}

impl InputSelectWire {
    /// Map to the `mmm-core` selector the file analyze path consumes.
    pub fn to_input_select(self) -> crate::analyze::InputSelect {
        match self {
            InputSelectWire::Auto => crate::analyze::InputSelect::Auto,
            InputSelectWire::Aligned => crate::analyze::InputSelect::Aligned,
            InputSelectWire::Solved => crate::analyze::InputSelect::Solved,
        }
    }
}

/// Geometry and (for [`JobMode::Solved`]) plate-solution metadata of one
/// input panel.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PanelDesc {
    /// Stable panel identifier, matching [`BandRequest::panel_id`].
    pub panel_id: u32,
    /// Panel width in pixels.
    pub width: u64,
    /// Panel height in pixels.
    pub height: u64,
    /// Channel count.
    pub channels: u64,
    /// XISF properties carried through from the input header; empty except
    /// in [`JobMode::Solved`], where the plate solution lives here.
    pub properties: Vec<XisfProperty>,
}

/// Request read by `mmm-ipc-worker --probe-panels` from stdin: a bare JSON
/// object (unframed, like `--probe-frame`). See PROTOCOL.md §11.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PanelProbeRequest {
    /// Exact release version of the worker binary the host expects, as in
    /// [`InitJob::worker_version`]. Checked first so a skewed module/worker
    /// pair fails at the probe — the very first worker contact of a run —
    /// with a clear message.
    pub worker_version: String,
    /// Panel file paths, one per panel, in `panels` order.
    pub paths: Vec<String>,
    /// Aligned-vs-solved override, as in [`JobMode::Files`]. Defaults to
    /// `Auto` when omitted.
    #[serde(default)]
    pub input_select: InputSelectWire,
}

/// One panel's header geometry in a [`PanelProbeReply`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct PanelProbeGeom {
    /// Panel width in pixels.
    pub width: u64,
    /// Panel height in pixels.
    pub height: u64,
    /// Channel count.
    pub channels: u64,
}

/// Reply printed by `--probe-panels` on stdout as one bare JSON object.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PanelProbeReply {
    /// Per-panel header geometry, in request `paths` order.
    pub panels: Vec<PanelProbeGeom>,
    /// `Some([w, h, ch])` — the worker's `choose_frame` result — when the
    /// job can resolve to solved mode (`input_select` is not `Aligned` and
    /// every panel carries a usable astrometric solution); `None` otherwise.
    /// Hosts size output slots by `max(max panel width, frame width)`.
    pub frame: Option<[u64; 3]>,
}

/// Wire form of [`BlendParams`]: plain data so it serializes with serde.
/// `mode` travels as a string (see [`Self::to_params`]) so the wire format
/// is stable across a `BlendMode` variant reorder; `surface_order` is not
/// part of `BlendParams` (it feeds the analyze stage) and is ignored by
/// `to_params`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct BlendParamsWire {
    /// Feather ramp length in canvas pixels.
    pub feather_px: f32,
    /// 1 = full resolution, 8 = blend from the L8 summaries (preview).
    pub downsample: u32,
    /// Output rows per band delivered to the sink.
    pub band_rows: u32,
    /// `"feather"`, `"twoband"`, or `"pyramid"` (unrecognized values map to
    /// `"pyramid"` in [`Self::to_params`]).
    pub mode: String,
    /// Optional region of interest in full-res canvas coords `[x0,y0,x1,y1]`.
    pub roi: Option<[u64; 4]>,
    /// Cross-panel defect veto in the two-band/pyramid detail stage.
    pub defect_veto: bool,
    /// Opt-in global background flatten polynomial order (`None` = off).
    pub flatten: Option<u32>,
    /// Surface-fit polynomial order for the analyze stage; not consumed by
    /// [`Self::to_params`].
    pub surface_order: Option<u32>,
    /// Write a seam/ownership map PNG (`seam_map.png`) into the session
    /// directory after a successful blend. Not part of [`BlendParams`];
    /// consumed by the worker's post-blend step. Defaults to `false` so
    /// params JSON from hosts that predate this field still parses.
    #[serde(default)]
    pub seam_map: bool,
    /// Photometric gain handling for the analyze stage: `"fit"` or
    /// `"unity"`. Not part of [`BlendParams`]; consumed by the worker via
    /// [`Self::gain_mode`]. Defaults to `"fit"` so params JSON from hosts
    /// that predate this field still parses.
    #[serde(default = "default_gain")]
    pub gain: String,
}

/// Serde default for [`BlendParamsWire::gain`]: params JSON from hosts that
/// predate the field still parses, and means the default fit solve.
fn default_gain() -> String {
    "fit".to_string()
}

impl Default for BlendParamsWire {
    fn default() -> Self {
        Self {
            feather_px: 256.0,
            downsample: 1,
            band_rows: 256,
            mode: "pyramid".to_string(),
            roi: None,
            defect_veto: true,
            flatten: None,
            surface_order: Some(2),
            seam_map: false,
            gain: "fit".to_string(),
        }
    }
}

impl BlendParamsWire {
    /// Converts wire parameters to [`BlendParams`]. `mode` maps
    /// `"feather"`/`"twoband"`/`"pyramid"` to the matching [`BlendMode`];
    /// any other string defaults to `Pyramid`. `surface_order` has no
    /// `BlendParams` counterpart and is dropped.
    pub fn to_params(&self) -> BlendParams {
        let mode = match self.mode.as_str() {
            "feather" => BlendMode::Feather,
            "twoband" => BlendMode::TwoBand,
            _ => BlendMode::Pyramid,
        };
        BlendParams {
            feather_px: self.feather_px,
            downsample: self.downsample,
            band_rows: self.band_rows as usize,
            mode,
            roi: self.roi,
            defect_veto: self.defect_veto,
            flatten: self.flatten,
        }
    }

    /// Maps the wire `gain` string to a [`GainMode`]: `"unity"` selects
    /// [`GainMode::Unity`]; `"fit"` and any unrecognized value map to
    /// [`GainMode::Fit`] (lenient like `mode`'s unrecognized → pyramid).
    pub fn gain_mode(&self) -> GainMode {
        match self.gain.as_str() {
            "unity" => GainMode::Unity,
            _ => GainMode::Fit,
        }
    }
}

/// The one-time job description sent from host to worker to start a run.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct InitJob {
    /// Wire-protocol version; the worker aborts on a mismatch with
    /// [`crate::ipc::IPC_PROTOCOL_VERSION`].
    pub protocol_version: u32,
    /// Exact release version of the `mmm-ipc-worker` binary the host expects
    /// (its `CARGO_PKG_VERSION`). The worker refuses the job on any
    /// mismatch: the module and worker ship as one package, and a skewed
    /// pair — e.g. a stale worker binary left behind by a partial update —
    /// could agree on the wire protocol yet disagree on its semantics, which
    /// the host has no way to detect once bands start flowing.
    pub worker_version: String,
    /// Name of the shared-memory segment carrying band slots.
    pub shm_name: String,
    /// Size in bytes of one band slot.
    pub slot_bytes: u64,
    /// Number of input slots in the shared-memory segment.
    pub input_slots: u32,
    /// Number of output slots in the shared-memory segment.
    pub output_slots: u32,
    /// Canvas dimensions as `[width, height, channels]`.
    pub canvas: [u64; 3],
    /// Per-panel geometry and metadata, in panel-id order.
    pub panels: Vec<PanelDesc>,
    /// How to read/align input panels.
    pub mode: JobMode,
    /// Session directory the worker should read/write cached artifacts from.
    pub session_dir: String,
    /// Blend parameters for the run.
    pub params: BlendParamsWire,
}

/// Messages sent from `mmm-ipc-worker` to the host.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum WorkerMsg {
    /// Requests input pixels for a band (see [`BandRequest`]).
    BandRequest(BandRequest),
    /// Reports progress within a pipeline stage.
    Progress {
        /// Stage name (e.g. `"blend"`).
        stage: String,
        /// Units completed so far.
        done: u64,
        /// Total units expected.
        total: u64,
    },
    /// Announces canvas geometry before output streaming begins.
    Begin {
        /// Canvas width in pixels.
        w: u64,
        /// Canvas height in pixels.
        h: u64,
        /// Channel count.
        ch: u64,
    },
    /// A blended output band is ready (see [`OutputBand`]).
    OutputBand(OutputBand),
    /// The job completed successfully; no further messages follow.
    Done,
    /// The job failed; no further messages follow.
    Error {
        /// Human-readable error description.
        message: String,
    },
}

/// Messages sent from the host to `mmm-ipc-worker`.
///
/// `Init` is sent exactly once per run (never on a hot path), so the size
/// gap to the other variants (dominated by `InitJob::panels`) is left
/// unboxed — boxing it would also break the `HostMsg::Init(got) => ...
/// assert_eq!(got, job)` pattern this protocol's tests rely on.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum HostMsg {
    /// Starts a run (see [`InitJob`]); always the first message.
    Init(InitJob),
    /// Replies to a [`WorkerMsg::BandRequest`].
    BandReply(BandReply),
    /// Acknowledges an [`WorkerMsg::OutputBand`], freeing its slot for reuse.
    OutputAck {
        /// The [`OutputBand::request_id`] being acknowledged.
        request_id: u32,
    },
    /// Aborts the run; the worker exits after cleaning up.
    Cancel,
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::WorkerMsg {}
    impl Sealed for super::HostMsg {}
}

/// Message envelopes that can be written with [`write_frame`]. Sealed: only
/// [`WorkerMsg`] and [`HostMsg`] implement it.
pub trait FrameBody: sealed::Sealed {
    /// Encodes this message to a `(tag, payload)` pair per the module-level
    /// frame layout.
    fn encode(&self) -> (u8, Vec<u8>);

    /// Checks this message can be safely represented on the wire before
    /// [`Self::encode`] runs. Only [`HostMsg::Init`] has anything to check
    /// (a finite-float precondition on its floats); every other message is
    /// valid by construction, so the default is a no-op.
    fn validate(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Serializes `msg` as the JSON payload for a frame whose tag already
/// disambiguates the variant. `serde_json` cannot fail for the data shapes
/// used in this protocol (strings, integers, options, and vecs thereof)
/// *provided* every `f32`/`f64` in `msg` is finite: JSON has no
/// representation for `NaN`/`Infinity`, so `serde_json` silently encodes
/// them as `null` instead of erroring here, which then fails to
/// *deserialize* on the far end with a confusing "invalid type: null,
/// expected f64" error nowhere near the real cause. `write_frame` enforces
/// that precondition via [`FrameBody::validate`] (see [`validate_finite`])
/// before this function is ever reached.
fn json_payload<T: Serialize + std::fmt::Debug>(msg: &T) -> Vec<u8> {
    serde_json::to_vec(msg).unwrap_or_else(|e| panic!("serialize {msg:?}: {e}"))
}

/// Checks every `f32`/`f64` reachable from `job` is finite (see
/// [`json_payload`] for why): [`BlendParamsWire::feather_px`], and each
/// panel's XISF [`PropertyValue::F64`]/`F64Vec`/`F64Mat` properties (the
/// only float-carrying data in [`InitJob`]; reachable in practice because
/// `formats::xisf`'s `Float64` property parser accepts `"nan"`/`"inf"`
/// text via `str::parse`).
fn validate_finite(job: &InitJob) -> io::Result<()> {
    if !job.params.feather_px.is_finite() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "InitJob.params.feather_px is not finite (NaN/Infinity)",
        ));
    }
    for panel in &job.panels {
        for prop in &panel.properties {
            let finite = match &prop.value {
                PropertyValue::F64(v) => v.is_finite(),
                PropertyValue::F64Vec(vs) => vs.iter().all(|v| v.is_finite()),
                PropertyValue::F64Mat { data, .. } => data.iter().all(|v| v.is_finite()),
                PropertyValue::Str(_) | PropertyValue::I64(_) | PropertyValue::Unread => true,
            };
            if !finite {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "InitJob panel {} property {:?} is not finite (NaN/Infinity)",
                        panel.panel_id, prop.id
                    ),
                ));
            }
        }
    }
    Ok(())
}

impl FrameBody for WorkerMsg {
    fn encode(&self) -> (u8, Vec<u8>) {
        match self {
            WorkerMsg::BandRequest(b) => (1, b.to_bytes()),
            WorkerMsg::Progress { .. } => (2, json_payload(self)),
            WorkerMsg::Begin { .. } => (3, json_payload(self)),
            WorkerMsg::OutputBand(b) => (4, b.to_bytes()),
            WorkerMsg::Done => (5, json_payload(self)),
            WorkerMsg::Error { .. } => (6, json_payload(self)),
        }
    }
}

impl FrameBody for HostMsg {
    fn encode(&self) -> (u8, Vec<u8>) {
        match self {
            HostMsg::Init(_) => (128, json_payload(self)),
            HostMsg::BandReply(b) => (129, b.to_bytes()),
            HostMsg::OutputAck { .. } => (130, json_payload(self)),
            HostMsg::Cancel => (131, json_payload(self)),
        }
    }

    fn validate(&self) -> io::Result<()> {
        match self {
            HostMsg::Init(job) => validate_finite(job),
            HostMsg::BandReply(_) | HostMsg::OutputAck { .. } | HostMsg::Cancel => Ok(()),
        }
    }
}

/// Writes one frame to `w`: `[u8 tag][u32 LE payload_len][payload]`.
///
/// Calls [`FrameBody::validate`] first, so a message that can't be safely
/// represented on the wire (currently: a non-finite float in a
/// [`HostMsg::Init`]) fails here with a clear `io::Error` instead of
/// silently corrupting the JSON payload.
pub fn write_frame<W: Write>(w: &mut W, msg: &impl FrameBody) -> io::Result<()> {
    msg.validate()?;
    let (tag, payload) = msg.encode();
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame payload too large"))?;
    w.write_all(&[tag])?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&payload)?;
    Ok(())
}

/// Reads the tag byte of the next frame, if any.
///
/// Returns `Ok(None)` only when EOF is hit before any byte of a new frame
/// (the peer closed its output cleanly). Any later EOF (mid-length or
/// mid-payload) means the pipe was cut mid-frame and surfaces as a
/// `io::ErrorKind::UnexpectedEof` from the subsequent `read_exact`.
fn read_tag<R: Read>(r: &mut R) -> io::Result<Option<u8>> {
    let mut tag = [0u8; 1];
    let n = r.read(&mut tag)?;
    Ok((n != 0).then_some(tag[0]))
}

/// Reads the `u32` LE length prefix and then that many payload bytes.
fn read_payload<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    // `len` is trusted, not attacker-bounded: both ends of this pipe are our
    // own processes (host and `mmm-ipc-worker`) talking over their own
    // stdin/stdout, not an external/adversarial input source.
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(payload)
}

fn json_error(e: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

fn unknown_tag(direction: &str, tag: u8) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unknown {direction} frame tag {tag}"),
    )
}

/// Reads one worker→host frame from `r`.
///
/// Returns `Ok(None)` on a clean EOF before any byte of a new frame (the
/// worker process exited). Truncation or malformed payloads propagate as
/// `io::Error`.
pub fn read_worker_frame<R: Read>(r: &mut R) -> io::Result<Option<WorkerMsg>> {
    let Some(tag) = read_tag(r)? else {
        return Ok(None);
    };
    let payload = read_payload(r)?;
    let msg = match tag {
        1 => WorkerMsg::BandRequest(BandRequest::from_bytes(&payload)?),
        2 | 3 | 5 | 6 => serde_json::from_slice(&payload).map_err(json_error)?,
        4 => WorkerMsg::OutputBand(OutputBand::from_bytes(&payload)?),
        other => return Err(unknown_tag("worker", other)),
    };
    Ok(Some(msg))
}

/// Reads one host→worker frame from `r`.
///
/// Returns `Ok(None)` on a clean EOF before any byte of a new frame (the
/// host closed the pipe). Truncation or malformed payloads propagate as
/// `io::Error`.
pub fn read_host_frame<R: Read>(r: &mut R) -> io::Result<Option<HostMsg>> {
    let Some(tag) = read_tag(r)? else {
        return Ok(None);
    };
    let payload = read_payload(r)?;
    let msg = match tag {
        128 => serde_json::from_slice(&payload).map_err(json_error)?,
        129 => HostMsg::BandReply(BandReply::from_bytes(&payload)?),
        130 | 131 => serde_json::from_slice(&payload).map_err(json_error)?,
        other => return Err(unknown_tag("host", other)),
    };
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_request_round_trips_through_a_pipe_buffer() {
        let req = BandRequest {
            request_id: 7,
            panel_id: 3,
            y0: 256,
            y1: 512,
            slot_id: 2,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &WorkerMsg::BandRequest(req.clone())).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        match read_worker_frame(&mut cur).unwrap().unwrap() {
            WorkerMsg::BandRequest(got) => assert_eq!(got, req),
            other => panic!("wrong variant: {other:?}"),
        }
        // A second read on the exhausted cursor is a clean EOF.
        assert!(read_worker_frame(&mut cur).unwrap().is_none());
    }

    #[test]
    fn init_job_json_round_trips() {
        let job = InitJob {
            protocol_version: crate::ipc::IPC_PROTOCOL_VERSION,
            worker_version: env!("CARGO_PKG_VERSION").to_string(),
            shm_name: "/mmm-test".into(),
            slot_bytes: 1 << 20,
            input_slots: 8,
            output_slots: 2,
            canvas: [100, 80, 3],
            panels: vec![PanelDesc {
                panel_id: 0,
                width: 100,
                height: 80,
                channels: 3,
                properties: vec![],
            }],
            mode: JobMode::Aligned,
            session_dir: "/tmp/x.mmm-session".into(),
            params: BlendParamsWire::default(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &HostMsg::Init(job.clone())).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        match read_host_frame(&mut cur).unwrap().unwrap() {
            HostMsg::Init(got) => assert_eq!(got, job),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    fn sample_init_job() -> InitJob {
        InitJob {
            protocol_version: crate::ipc::IPC_PROTOCOL_VERSION,
            worker_version: env!("CARGO_PKG_VERSION").to_string(),
            shm_name: "/mmm-test".into(),
            slot_bytes: 1 << 20,
            input_slots: 1,
            output_slots: 1,
            canvas: [10, 10, 1],
            panels: vec![PanelDesc {
                panel_id: 0,
                width: 10,
                height: 10,
                channels: 1,
                properties: vec![],
            }],
            mode: JobMode::Aligned,
            session_dir: "/tmp/x.mmm-session".into(),
            params: BlendParamsWire::default(),
        }
    }

    #[test]
    fn truncated_payload_is_an_error_not_a_false_eof() {
        let req = BandRequest {
            request_id: 1,
            panel_id: 1,
            y0: 0,
            y1: 1,
            slot_id: 0,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &WorkerMsg::BandRequest(req)).unwrap();
        buf.truncate(buf.len() - 1); // cut the frame mid-payload
        let mut cur = std::io::Cursor::new(buf);
        let err = read_worker_frame(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn unknown_tag_byte_is_an_error() {
        let mut buf = Vec::new();
        buf.push(200u8); // not a tag any WorkerMsg variant uses
        buf.extend_from_slice(&0u32.to_le_bytes()); // zero-length payload
        let mut cur = std::io::Cursor::new(buf);
        let err = read_worker_frame(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn write_frame_rejects_non_finite_feather_px() {
        let mut job = sample_init_job();
        job.params.feather_px = f32::NAN;
        let mut buf = Vec::new();
        let err = write_frame(&mut buf, &HostMsg::Init(job)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn write_frame_rejects_non_finite_property_float() {
        let mut job = sample_init_job();
        job.panels[0].properties.push(XisfProperty {
            id: "PCL:Test".into(),
            type_: "Float64".into(),
            value: PropertyValue::F64(f64::NAN),
            location: None,
        });
        let mut buf = Vec::new();
        let err = write_frame(&mut buf, &HostMsg::Init(job)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn files_mode_carries_input_select_and_round_trips() {
        use super::*;
        let m = JobMode::Files {
            paths: vec!["a.xisf".into(), "b.xisf".into()],
            input_select: InputSelectWire::Solved,
        };
        let js = serde_json::to_string(&m).unwrap();
        // externally-tagged: the enum value round-trips exactly.
        let back: JobMode = serde_json::from_str(&js).unwrap();
        assert_eq!(m, back);
        // InputSelectWire serializes as a bare string (unit variant).
        assert_eq!(
            serde_json::to_string(&InputSelectWire::Auto).unwrap(),
            "\"Auto\""
        );
    }

    #[test]
    fn blend_params_seam_map_defaults_off_and_round_trips() {
        // Params JSON from a host that predates seam_map (no key) must still
        // parse, with the seam map off.
        let legacy = serde_json::json!({
            "feather_px": 256.0,
            "downsample": 1,
            "band_rows": 256,
            "mode": "pyramid",
            "roi": null,
            "defect_veto": true,
            "flatten": null,
            "surface_order": 2
        });
        let p: BlendParamsWire = serde_json::from_value(legacy).unwrap();
        assert!(!p.seam_map);

        let on = BlendParamsWire {
            seam_map: true,
            ..Default::default()
        };
        let back: BlendParamsWire =
            serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
        assert!(back.seam_map);
    }

    #[test]
    fn blend_params_gain_defaults_fit_and_maps() {
        use crate::photometry::GainMode;
        // Params JSON from a host that predates the gain field (no key) must
        // still parse, selecting the default fit solve.
        let legacy = serde_json::json!({
            "feather_px": 256.0,
            "downsample": 1,
            "band_rows": 256,
            "mode": "pyramid",
            "roi": null,
            "defect_veto": true,
            "flatten": null,
            "surface_order": 2
        });
        let p: BlendParamsWire = serde_json::from_value(legacy).unwrap();
        assert_eq!(p.gain, "fit");
        assert_eq!(p.gain_mode(), GainMode::Fit);

        let unity = BlendParamsWire {
            gain: "unity".to_string(),
            ..Default::default()
        };
        assert_eq!(unity.gain_mode(), GainMode::Unity);
        let back: BlendParamsWire =
            serde_json::from_str(&serde_json::to_string(&unity).unwrap()).unwrap();
        assert_eq!(back.gain_mode(), GainMode::Unity);

        // Unrecognized strings degrade to fit (mirrors mode -> "pyramid").
        let odd = BlendParamsWire {
            gain: "warp".to_string(),
            ..Default::default()
        };
        assert_eq!(odd.gain_mode(), GainMode::Fit);
    }

    #[test]
    fn input_select_wire_maps_to_core() {
        use super::InputSelectWire as W;
        use crate::analyze::InputSelect as I;
        assert_eq!(W::Auto.to_input_select(), I::Auto);
        assert_eq!(W::Aligned.to_input_select(), I::Aligned);
        assert_eq!(W::Solved.to_input_select(), I::Solved);
    }
}
