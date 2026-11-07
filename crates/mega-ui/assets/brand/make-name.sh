#!/bin/sh
# Regenerate an app-name image for the brand lockup from the Angora display
# face. Astrometrical holds a commercial licence for Angora; the font file
# itself is not committed. Kerning 31 at 96 pt is the +0.32em tracking of
# the web's wordmark; the output is white RGBA, about 70 px tall.
#
# usage: make-name.sh ANGORA_OTF [TEXT [OUT]]
#   e.g. make-name.sh ~/Branding/Angora.otf "MEGA MERGE MOSAIC" name-mega-merge-mosaic.png
set -eu
font=${1:?usage: make-name.sh ANGORA_OTF [TEXT [OUT]]}
text=${2:-MEGA MERGE MOSAIC}
out=${3:-name-mega-merge-mosaic.png}
magick -background none -fill white -font "$font" -pointsize 96 -kerning 31 \
    label:"$text" -trim +repage "PNG32:$out"
