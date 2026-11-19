#!/bin/sh
# Fetches the bundled hardware docs and renders them to page PNGs the GUI's
# DOCS tab displays. Needs: curl, pdftoppm (poppler), and chromium (only for
# the beginner's guide, which ships as a web page).
#
# usage: tools/fetch-docs.sh [docs-dir]   (default: ~/3do-library/docs)
set -eu

DOCS_DIR="${1:-$HOME/3do-library/docs}"
BASE="https://3dodev.com/_media/documentation/manuals"
GUIDE_URL="https://racketboy.com/retro/3do-interactive-multiplayer-101-a-beginners-guide"

fetch() { # slug url dpi
    slug="$1"; url="$2"; dpi="$3"
    mkdir -p "$DOCS_DIR/$slug"
    case "$url" in
        *.pdf) curl -fsSL "$url" -o "$DOCS_DIR/$slug/source.pdf" ;;
        *) chromium --headless --disable-gpu --no-pdf-header-footer \
               --print-to-pdf="$DOCS_DIR/$slug/source.pdf" "$url" ;;
    esac
    pdftoppm -png -r "$dpi" "$DOCS_DIR/$slug/source.pdf" "$DOCS_DIR/$slug/page"
    echo "$slug: $(ls "$DOCS_DIR/$slug" | grep -c 'page-.*png') pages"
}

fetch fz1-technical-guide "$BASE/panasonic_fz-1_-_technical_guide_us.pdf" 140
fetch fz1-service-manual  "$BASE/panasonic_fz-1_-_service_manual.pdf" 140
fetch fz1-operating-manual "https://github.com/trapexit/3do-docs/raw/master/hardware/manuals/panasonic_fz-1_-_operating_manual_eu.pdf" 140
fetch beginners-guide     "$GUIDE_URL" 120
