//! The repository NOTICE credits every embedded font under its licences.

const NOTICE: &str = include_str!("../../../NOTICE");

#[test]
fn notice_lists_every_font_licence() {
    for needle in [
        "Inter 4.1",
        "JetBrains Mono 2.304",
        "SIL Open Font License 1.1",
        "Lucide",
        "ISC",
    ] {
        assert!(NOTICE.contains(needle), "NOTICE mentions {needle}");
    }
    // Lucide's licence also carries MIT terms for icons derived from
    // Feather (Cole Bemis), several of which we use.
    assert!(NOTICE.contains("Feather"), "NOTICE credits Feather");
    assert!(NOTICE.contains("MIT"), "NOTICE names the MIT licence");
}
