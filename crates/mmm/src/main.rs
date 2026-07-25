use clap::{Parser, Subcommand};

/// Mega Merge Mosaic — fast merging/blending of pre-aligned astro mosaic panels.
#[derive(Parser)]
#[command(name = "mmm", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase log verbosity (-v, -vv)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Command {
    /// Print header metadata for panel files (and optionally quick pixel stats)
    Info {
        /// Input panel files (FITS/XISF)
        #[arg(required = true)]
        panels: Vec<std::path::PathBuf>,

        /// Also scan pixel data for min/max/zero-fraction (reads whole file)
        #[arg(long)]
        stats: bool,
    },

    /// Analyze panels: build tiled cache, coverage masks, and the overlap graph
    Analyze {
        /// Input panel files (XISF): pre-aligned full-canvas frames, or
        /// unaligned plate-solved panels (reprojected automatically)
        #[arg(required = true)]
        panels: Vec<std::path::PathBuf>,

        /// Session directory for cached analysis (created if missing)
        #[arg(short, long, default_value = "mosaic.mmm-session")]
        session: std::path::PathBuf,

        /// Residual surface correction: off, 0 (constant), 1 (plane), 2 (quadratic)
        #[arg(long, default_value = "2")]
        surface: String,

        /// Input kind: auto (detect), aligned (registered full-canvas
        /// frames), solved (unaligned panels with astrometric solutions)
        #[arg(long, default_value = "auto")]
        input: String,
    },

    /// Report analysis results: the overlap-graph edge table
    Report {
        /// Session directory produced by `mmm analyze`
        #[arg(short, long, default_value = "mosaic.mmm-session")]
        session: std::path::PathBuf,

        /// Write a seam/ownership map PNG: autostretched blended preview
        /// with owner regions tinted, seams drawn dark, panel ids labelled
        #[arg(long)]
        seam_png: Option<std::path::PathBuf>,
    },

    /// Blend the analyzed panels into a mosaic FITS (and optional PNG preview)
    Blend {
        /// Session directory produced by `mmm analyze`
        #[arg(short, long, default_value = "mosaic.mmm-session")]
        session: std::path::PathBuf,

        /// Output FITS file (BITPIX=-32, planar channels)
        #[arg(short, long)]
        output: std::path::PathBuf,

        /// Downsample factor: 1 = full resolution, 8 = fast preview from L8 summaries
        #[arg(long, default_value_t = 1)]
        downsample: u32,

        /// Feather ramp length in canvas pixels
        #[arg(long, default_value_t = 256.0)]
        feather: f32,

        /// Blend mode: pyramid (multiband base + star-safe seams, default),
        /// twoband (feathered base + star-safe seams) or feather (phase-1)
        #[arg(long, default_value = "pyramid")]
        mode: String,

        /// Also write an autostretched 8-bit PNG preview (downsampled runs only)
        #[arg(long)]
        png: Option<std::path::PathBuf>,

        /// Region of interest in full-res canvas pixels: x,y,w,h
        #[arg(long)]
        roi: Option<String>,

        /// Cross-panel defect veto in overlaps (twoband mode): suppresses
        /// cosmic-ray residue and satellite trails that survive in one panel
        #[arg(long, default_value = "on")]
        defect_veto: String,

        /// Opt-in global background flatten: off (default), 1 (plane) or
        /// 2 (quadratic). Fits the merged mosaic's background and subtracts
        /// its varying part, preserving the central level; refuses on
        /// signal-dominated (nebula-heavy) mosaics
        #[arg(long, default_value = "off")]
        flatten: String,
    },
}

fn parse_roi(s: &str) -> anyhow::Result<[u64; 4]> {
    let v: Vec<u64> = s.split(',').map(|p| p.trim().parse()).collect::<Result<_, _>>()?;
    let [x, y, w, h] = v.as_slice() else {
        anyhow::bail!("--roi must be x,y,w,h (got '{s}')");
    };
    anyhow::ensure!(*w > 0 && *h > 0, "--roi width/height must be > 0");
    Ok([*x, *y, x + w, y + h])
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| level.into()),
        )
        .init();

    match cli.command {
        Command::Info { panels, stats } => {
            for path in &panels {
                info_panel(path, stats)?;
            }
            Ok(())
        }
        Command::Analyze { panels, session, surface, input } => {
            tracing::info!(?session, n_panels = panels.len(), "analyze requested");
            let surface_order = match surface.as_str() {
                "off" => None,
                "0" => Some(0),
                "1" => Some(1),
                "2" => Some(2),
                other => anyhow::bail!("--surface must be off, 0, 1 or 2 (got {other})"),
            };
            let input = match input.as_str() {
                "auto" => mmm_core::analyze::InputSelect::Auto,
                "aligned" => mmm_core::analyze::InputSelect::Aligned,
                "solved" => mmm_core::analyze::InputSelect::Solved,
                other => anyhow::bail!("--input must be auto, aligned or solved (got {other})"),
            };
            let t0 = std::time::Instant::now();
            let s = mmm_core::analyze::analyze_input(&panels, &session, surface_order, input)?;
            match (&s.frame, s.align_secs) {
                (Some(f), align_secs) => println!(
                    "input: solved panels — {} reprojected onto a fresh {}x{} frame \
                     ({:.3}\"/px, center RA {:.4} Dec {:+.4}) in {:.2}s",
                    s.panels.len(),
                    f.width,
                    f.height,
                    f.scale_deg * 3600.0,
                    f.crval[0],
                    f.crval[1],
                    align_secs.unwrap_or(0.0),
                ),
                _ => println!("input: aligned full-canvas frames"),
            }
            let (w, h, ch) = s.canvas;
            println!("canvas: {w}x{h} x{ch}ch   session: {}", s.dir.display());
            println!(
                "{:>3}  {:<28} {:>26} {:>8}  per-channel mean",
                "id", "file", "bbox [x0,x1)x[y0,y1)", "nonzero"
            );
            for p in &s.panels {
                let name = panel_name(p);
                let bbox = format!(
                    "[{},{})x[{},{})",
                    p.bbox[0], p.bbox[2], p.bbox[1], p.bbox[3]
                );
                let means = p
                    .ch_mean
                    .iter()
                    .map(|m| format!("{m:.6}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "{:>3}  {:<28} {:>26} {:>7.1}%  {}",
                    p.id,
                    name,
                    bbox,
                    100.0 * p.nonzero_frac,
                    means
                );
            }
            println!("analyze: {:.2}s", t0.elapsed().as_secs_f64());
            Ok(())
        }
        Command::Report { session, seam_png } => report(&session, seam_png.as_deref()),
        Command::Blend { session, output, downsample, feather, mode, png, roi, defect_veto, flatten } => {
            let mode = match mode.as_str() {
                "feather" => mmm_core::blend::BlendMode::Feather,
                "twoband" => mmm_core::blend::BlendMode::TwoBand,
                "pyramid" => mmm_core::blend::BlendMode::Pyramid,
                other => anyhow::bail!("--mode must be pyramid, twoband or feather (got {other})"),
            };
            let defect_veto = match defect_veto.as_str() {
                "on" => true,
                "off" => false,
                other => anyhow::bail!("--defect-veto must be on or off (got {other})"),
            };
            let flatten = match flatten.as_str() {
                "off" => None,
                "1" => Some(1),
                "2" => Some(2),
                other => anyhow::bail!("--flatten must be off, 1 or 2 (got {other})"),
            };
            let roi = roi.as_deref().map(parse_roi).transpose()?;
            blend_cmd(
                &session,
                &output,
                downsample,
                feather,
                mode,
                png.as_deref(),
                roi,
                defect_veto,
                flatten,
            )
        }
    }
}

/// Display name for a panel: the original input file for reprojected panels
/// (whose `path` points at the session cache), else the panel file itself.
fn panel_name(p: &mmm_core::session::PanelMeta) -> String {
    let path = p.source.as_ref().unwrap_or(&p.path);
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// FITS cards describing the *input panel's* geometry or pointing. These
/// must not pass through to a solved-session output: its canvas is a fresh
/// [`mmm_core::align::MosaicFrame`], and the correct cards are emitted from
/// that frame instead. Non-geometric metadata (EXPTIME, INSTRUME, …) still
/// passes through from panel 0.
fn geometry_card(name: &str) -> bool {
    let n = name.trim().to_ascii_uppercase();
    matches!(
        n.as_str(),
        "RA" | "DEC"
            | "OBJCTRA"
            | "OBJCTDEC"
            | "RADESYS"
            | "EQUINOX"
            | "EPOCH"
            | "LONPOLE"
            | "LATPOLE"
    ) || ["CRVAL", "CRPIX", "CDELT", "CROTA", "CTYPE", "CUNIT", "CD1_", "CD2_", "PC1_", "PC2_", "PV1_", "PV2_"]
        .iter()
        .any(|p| n.starts_with(p))
}

#[allow(clippy::too_many_arguments)]
fn blend_cmd(
    session_dir: &std::path::Path,
    output: &std::path::Path,
    downsample: u32,
    feather: f32,
    mode: mmm_core::blend::BlendMode,
    png: Option<&std::path::Path>,
    roi: Option<[u64; 4]>,
    defect_veto: bool,
    flatten: Option<u32>,
) -> anyhow::Result<()> {
    use mmm_core::astrometry::{wcs_cards, wcs_from_properties};
    use mmm_core::blend::{BlendParams, blend, output_bbox};
    use mmm_core::formats::xisf::XisfPanel;
    use mmm_core::output::Tee;
    use mmm_core::output::fits::{FitsSink, keywords_for_output};
    use mmm_core::output::png::PngSink;

    let t0 = std::time::Instant::now();
    let session = mmm_core::session::Session::open(session_dir)?;
    let graph = mmm_core::overlap::OverlapGraph::load(&session.overlap_graph_path())?;
    let phot = mmm_core::photometry::Photometry::load(&session.photometry_path())?;
    let params_for_bbox = BlendParams {
        feather_px: feather,
        downsample,
        mode,
        roi,
        defect_veto,
        flatten,
        ..Default::default()
    };
    let bbox = output_bbox(&session, &params_for_bbox)?;
    let ds = downsample.max(1) as u64;
    // Crop origin in output pixel units (best-effort for downsampled previews:
    // the WCS scale itself is not rewritten).
    let crop = (bbox[0] / ds, bbox[1] / ds);
    // Panel-0 file for FITS keyword passthrough: for solved sessions the
    // panel path is the reprojection cache, so the original input is used.
    let p0 = &session.panels[0];
    let ref_panel = XisfPanel::open(p0.source.as_ref().unwrap_or(&p0.path))?;
    let mut keywords = keywords_for_output(&ref_panel.header().fits_keywords, crop);
    if session.frame.is_some() {
        // Solved session: the output canvas is a fresh frame — panel-0
        // geometry/pointing cards would lie about it and are dropped.
        keywords.retain(|kw| !geometry_card(&kw.name));
    }
    // Astrometric solution lives in XISF properties, not FITS keywords; attach
    // real WCS cards at full resolution (downsampled previews would need a
    // rescaled CD matrix — not worth lying about; skip them there). Solved
    // sessions use the session's own mosaic frame, never panel-0 passthrough.
    if ds == 1 {
        let wcs = match &session.frame {
            Some(frame) => {
                println!("wcs: session mosaic frame (solved input)");
                Some(frame.linear_wcs())
            }
            None => wcs_from_properties(&ref_panel.header().properties),
        };
        match wcs {
            Some(wcs) => {
                // Output height fixes the bottom-up y reflection of the cards.
                keywords.extend(wcs_cards(&wcs, (bbox[0], bbox[1]), bbox[3] - bbox[1]));
                println!("wcs: cards attached");
            }
            None => println!("wcs: no astrometric solution found in panel 0"),
        }
    }
    println!(
        "session: {} panels, canvas {}x{}x{}ch, output bbox [{},{})x[{},{})  ({:.2}s)",
        session.panels.len(),
        session.canvas.0,
        session.canvas.1,
        session.canvas.2,
        bbox[0],
        bbox[2],
        bbox[1],
        bbox[3],
        t0.elapsed().as_secs_f64()
    );

    let surfaces = if session.surfaces_path().exists() {
        Some(mmm_core::surfaces::Surfaces::load(&session.surfaces_path())?)
    } else {
        None
    };
    match &surfaces {
        Some(s) => println!("surfaces: applying residual corrections (order {})", s.order),
        None => println!("surfaces: none (analyze ran with --surface off)"),
    }
    match flatten {
        Some(o) => println!("flatten: order {o} global background flatten (opt-in)"),
        None => println!("flatten: off"),
    }

    let params = params_for_bbox;
    let t1 = std::time::Instant::now();
    let mut fits = FitsSink::create(output, keywords)?;
    match png {
        Some(png_path) => {
            let mut png_sink = PngSink::create(png_path);
            let mut tee = Tee::new(&mut fits, &mut png_sink);
            blend(&session, &phot, surfaces.as_ref(), &graph, &params, &mut tee)?;
            println!("png preview: {}", png_path.display());
        }
        None => blend(&session, &phot, surfaces.as_ref(), &graph, &params, &mut fits)?,
    }
    println!("blend + write: {:.2}s", t1.elapsed().as_secs_f64());

    let out_w = bbox[2].div_ceil(ds) - bbox[0] / ds;
    let out_h = bbox[3].div_ceil(ds) - bbox[1] / ds;
    println!(
        "output: {} ({}x{} x{}ch, downsample {downsample}, feather {feather} px, {mode:?})",
        output.display(),
        out_w,
        out_h,
        session.canvas.2
    );
    println!("total: {:.2}s", t0.elapsed().as_secs_f64());
    Ok(())
}

fn report(session_dir: &std::path::Path, seam_png: Option<&std::path::Path>) -> anyhow::Result<()> {
    use mmm_core::overlap::OverlapGraph;
    use mmm_core::session::Session;

    let session = Session::open(session_dir)?;
    let graph = OverlapGraph::load(&session.overlap_graph_path())?;
    let (w, h, ch) = session.canvas;
    println!(
        "canvas: {w}x{h} x{ch}ch   panels: {}   session: {}",
        session.panels.len(),
        session.dir.display()
    );

    let phot = mmm_core::photometry::Photometry::load(&session.photometry_path()).ok();
    let surfaces = mmm_core::surfaces::Surfaces::load(&session.surfaces_path()).ok();

    // Seam diagnostics need the summaries + owner map (same feather as the
    // blend's default); skipped when photometry is missing.
    let feather = mmm_core::blend::BlendParams::default().feather_px;
    let mut deltas = None;
    if let Some(phot) = &phot {
        let (summaries, owner) =
            mmm_core::diag::load_owner_map(&session, &graph, phot, surfaces.as_ref(), feather)?;
        deltas = Some(mmm_core::diag::seam_deltas(
            &summaries,
            &owner,
            phot,
            surfaces.as_ref(),
            session.canvas,
        ));
        drop(summaries);
        if let Some(png) = seam_png {
            mmm_core::diag::write_seam_map(
                &session,
                &graph,
                phot,
                surfaces.as_ref(),
                &owner,
                feather,
                png,
            )?;
            println!("seam map: {}", png.display());
        }
    } else if seam_png.is_some() {
        anyhow::bail!("--seam-png needs photometry results (re-run `mmm analyze`)");
    }

    // Median seam Δ for the ⚠ flag.
    let seam_median = deltas.as_ref().map_or(0.0, |m| {
        let mut v: Vec<f64> = m.values().filter(|d| d.n > 0).map(|d| d.delta).collect();
        v.sort_by(f64::total_cmp);
        if v.is_empty() { 0.0 } else { v[v.len() / 2] }
    });

    let name = |id: usize| -> String {
        session.panels.get(id).map(panel_name).unwrap_or_else(|| format!("panel {id}"))
    };
    println!(
        "\noverlap edges ({}; seam Δ = mean |corrected step| across owner boundary, ⚠ > 3× median):",
        graph.edges.len()
    );
    println!(
        "{:>3}-{:<3} {:>8} {:>12} {:>10}  {:<24} files",
        "a", "b", "cells", "~px", "seam Δ", "bbox8 [x0,x1)x[y0,y1)"
    );
    for e in &graph.edges {
        let bbox = format!(
            "[{},{})x[{},{})",
            e.bbox8[0], e.bbox8[2], e.bbox8[1], e.bbox8[3]
        );
        let sd = deltas
            .as_ref()
            .and_then(|m| m.get(&(e.a.min(e.b), e.a.max(e.b))))
            .filter(|d| d.n > 0);
        let (delta, flag) = match sd {
            Some(d) => (
                format!("{:.3e}", d.delta),
                if seam_median > 0.0 && d.delta > 3.0 * seam_median { "  ⚠ seam" } else { "" },
            ),
            None => ("-".to_string(), ""),
        };
        println!(
            "{:>3}-{:<3} {:>8} {:>12} {:>10}  {:<24} {} | {}{}",
            e.a,
            e.b,
            e.n_cells,
            e.n_cells * 64,
            delta,
            bbox,
            name(e.a),
            name(e.b),
            flag
        );
    }

    let comps = graph.components(session.panels.len());
    println!("\nconnected components: {}", comps.len());
    for (i, comp) in comps.iter().enumerate() {
        let ids = comp.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(" ");
        println!("  #{i}: {ids}");
    }

    match &phot {
        Some(phot) => report_photometry(phot, &name),
        None => println!("\nno photometry results (re-run `mmm analyze`)"),
    }

    match &surfaces {
        Some(surf) => report_surfaces(surf, &name),
        None => println!("\nno residual surfaces (analyze ran with --surface off)"),
    }
    Ok(())
}

fn report_surfaces(surf: &mmm_core::surfaces::Surfaces, name: &dyn Fn(usize) -> String) {
    use mmm_core::surfaces::WARN_MAD_FACTOR;

    let channels = surf.coeffs.len();
    let n_panels = surf.coeffs.first().map(|c| c.len()).unwrap_or(0);
    println!(
        "\nresidual surfaces (order {}; ⚠ = max|s| > {WARN_MAD_FACTOR}× background MAD):",
        surf.order
    );
    let mut header = format!("{:>3} ", "id");
    for c in 0..channels {
        header.push_str(&format!(" {:>10}{c} {:>10}{c}", "max|s|", "bgMAD"));
    }
    println!("{header}  file");
    for p in 0..n_panels {
        let mut row = format!("{p:>3} ");
        let mut warn = false;
        for c in 0..channels {
            let s = surf.max_abs_s.get(c).and_then(|v| v.get(p)).copied().unwrap_or(0.0);
            let mad = surf.bg_mad.get(c).and_then(|v| v.get(p)).copied().unwrap_or(0.0);
            warn |= mad > 0.0 && s > WARN_MAD_FACTOR * mad;
            row.push_str(&format!(" {s:>11.3e} {mad:>11.3e}"));
        }
        let flag = if warn { "  ⚠ runaway correction" } else { "" };
        println!("{row}  {}{flag}", name(p));
    }
}

fn report_photometry(phot: &mmm_core::photometry::Photometry, name: &dyn Fn(usize) -> String) {
    let channels = phot.gains.len();

    // Per-channel median rms for the suspect-edge flag.
    let median_rms: Vec<f64> = (0..channels)
        .map(|c| {
            let mut r: Vec<f64> = phot
                .edge_fits
                .iter()
                .filter(|f| f.channel as usize == c)
                .map(|f| f.rms)
                .collect();
            r.sort_by(f64::total_cmp);
            if r.is_empty() {
                0.0
            } else if r.len() % 2 == 1 {
                r[r.len() / 2]
            } else {
                0.5 * (r[r.len() / 2 - 1] + r[r.len() / 2])
            }
        })
        .collect();

    println!("\nphotometric edge fits (I_b ≈ gain·I_a + offset; ⚠ = rms > 3× channel median):");
    println!(
        "{:>3}-{:<3} {:>2} {:>9} {:>10} {:>10} {:>7}",
        "a", "b", "ch", "gain", "offset", "rms", "n"
    );
    for f in &phot.edge_fits {
        let med = median_rms.get(f.channel as usize).copied().unwrap_or(0.0);
        let flag = if med > 0.0 && f.rms > 3.0 * med { "  ⚠ suspect" } else { "" };
        println!(
            "{:>3}-{:<3} {:>2} {:>9.4} {:>10.6} {:>10.3e} {:>7}{}",
            f.a, f.b, f.channel, f.gain, f.offset, f.rms, f.n, flag
        );
    }

    let n_panels = phot.gains.first().map(|g| g.len()).unwrap_or(0);
    println!("\nper-panel corrections (I' = g·I + o), per channel:");
    let mut header = format!("{:>3} ", "id");
    for c in 0..channels {
        header.push_str(&format!(" {:>8}{c} {:>10}{c}", "g", "o"));
    }
    println!("{header}  file");
    for p in 0..n_panels {
        let mut row = format!("{p:>3} ");
        for c in 0..channels {
            row.push_str(&format!(" {:>9.4} {:>+11.6}", phot.gains[c][p], phot.offsets[c][p]));
        }
        println!("{row}  {}", name(p));
    }
}

fn info_panel(path: &std::path::Path, stats: bool) -> anyhow::Result<()> {
    use mmm_core::formats::xisf::XisfPanel;

    let panel = XisfPanel::open(path)?;
    let h = panel.header();
    println!("{}", path.display());
    println!(
        "  geometry: {}x{} x{}ch  {:?}  data @ {} ({} bytes)",
        h.width, h.height, h.channels, h.sample_format, h.data_offset, h.data_size
    );
    for kw in &h.fits_keywords {
        if matches!(kw.name.as_str(), "OBJECT" | "RA" | "DEC" | "INSTRUME" | "BAYERPAT" | "EXPTIME") {
            println!("  {:8} = {}", kw.name, kw.value);
        }
    }

    if stats {
        panel.advise_sequential();
        let t0 = std::time::Instant::now();
        for c in 0..panel.channels() {
            let plane = panel.channel(c);
            let (mut min, mut max, mut zeros, mut sum) = (f32::INFINITY, f32::NEG_INFINITY, 0u64, 0f64);
            for &v in plane {
                if v == 0.0 {
                    zeros += 1;
                } else {
                    min = min.min(v);
                    max = max.max(v);
                    sum += v as f64;
                }
            }
            let n = plane.len() as u64;
            let nonzero = n - zeros;
            println!(
                "  ch{c}: nonzero {:.1}%  min {:.6}  max {:.6}  mean {:.6}",
                100.0 * nonzero as f64 / n as f64,
                min,
                max,
                if nonzero > 0 { sum / nonzero as f64 } else { 0.0 }
            );
        }
        println!("  stats scan: {:.2}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}
