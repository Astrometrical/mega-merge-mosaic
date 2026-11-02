//! Test helpers: small synthetic mosaics (see [`crate::synth_mosaic`]),
//! never `test_data/`.

use std::path::Path;

use crate::session::OpenSession;
pub(crate) use crate::synth_mosaic::CANVAS;

/// A grouped Ha + OIII mosaic session in `root/m.mmm-session`, grouped by
/// the `FILTER` header, not yet analyzed.
pub(crate) fn two_group_session(root: &Path) -> OpenSession {
    let mosaic = crate::synth_mosaic::two_group_mosaic(root);
    let groups = mosaic.groups().ok();
    OpenSession {
        mosaic,
        canvas: None,
        groups,
        lock: None,
    }
}
