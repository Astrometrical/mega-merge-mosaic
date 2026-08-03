// MmmIcon.h -- SVG artwork for the MegaMergeMosaic process.
//
// MMM_PROCESS_ICON_SVG is returned by MetaProcess::IconImageSVG() /
// ProcessInterface::IconImageSVG() and is what appears in the Process
// Explorer, process icons and the tool window's title bar.
//
// EDIT THIS FILE to change the icon. To preview outside PixInsight, copy the
// literal between the svg( )svg delimiters into a .svg file and open it in a
// browser.
//
// Motif: four offset mosaic panels in the Astrometrical teal ramp
// (#1d7588 / #2a95ab / #4ebcd4) behind the Astrometrical chevron mark.
//
// The chevron path is the actual Astrometrical "A" mark (the Angora font
// glyph): polygon vertices traced from apps/web/src/assets/Logo-Black.png in
// the astrometrical repo -- pointed apex, THIN left stroke, THICK right
// stroke, notch rising to about 60% height. Do not "fix" the asymmetry.

#ifndef __MmmIcon_h
#define __MmmIcon_h

#define MMM_PROCESS_ICON_SVG R"svg(<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" width="48" height="48">
  <!-- mosaic panels: four tiles, slightly offset like registered frames -->
  <rect x="3"  y="5"  width="21" height="21" rx="2" fill="#1d7588"/>
  <rect x="25" y="3"  width="20" height="21" rx="2" fill="#2a95ab"/>
  <rect x="5"  y="27" width="20" height="18" rx="2" fill="#2a95ab"/>
  <rect x="26" y="25" width="19" height="20" rx="2" fill="#4ebcd4"/>
  <!-- panel seams -->
  <rect x="3" y="3" width="42" height="42" rx="2" fill="none" stroke="#0b3541" stroke-opacity="0.35" stroke-width="1"/>
  <!-- Astrometrical chevron: the Angora "A" mark, traced from Logo-Black.png -->
  <path d="M 23.82 10.14 L 42 37.86 L 32.91 37.86 L 21.13 19.89 L 9.55 37.86 L 6 37.86 Z"
        fill="#ffffff" stroke="#0b3541" stroke-width="1.2" stroke-linejoin="round"/>
</svg>
)svg"

// Plain chevron mark for the interface header (no tiles), Astrometrical teal.
// Same traced Angora "A" glyph at full size.
#define MMM_CHEVRON_SVG R"svg(<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" width="48" height="48">
  <path d="M 23.78 7.06 L 46 40.94 L 34.89 40.94 L 20.49 18.97 L 6.34 40.94 L 2 40.94 Z" fill="#2a95ab"/>
</svg>
)svg"

#endif   // __MmmIcon_h
