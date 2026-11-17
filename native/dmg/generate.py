#!/usr/bin/env python3
"""Render ClipKeep's macOS DMG background and a Finder-window preview.

Window is 760×527 to match a spacious installer like Grok Bot.
Icon slots stay empty: Finder draws clipkeep.app at (225, 229) and
Applications at (525, 229), both 128px (Tauri's default).
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
ICON_PATH = REPO / "native" / "icons" / "icon.icns"
NATIVE_FOLDER = Path("/tmp/macos-icons/ApplicationsFolderIcon.png")
OUT_PNG = ROOT / "background.png"
OUT_TIFF = ROOT / "background.tiff"
OUT_PREVIEW = ROOT / "preview.png"

# 1x window, same as Finder WindowBounds
WIN_W, WIN_H = 760, 527
SCALE = 2
TITLEBAR = 28  # compact Finder title bar; content view clips this much

BG = (252, 252, 252, 255)
BLUE = (37, 99, 235, 255)
NOTE = (110, 114, 122, 255)
LABEL = (55, 55, 57, 255)

FONT_REG = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"
FONT_BOLD = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc"
SC = 2

APP_POS = (225, 229)
APPS_POS = (525, 229)
ICON_SIZE = 128


def font(path: str, size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(path, size, index=SC)


def paste_center(dst: Image.Image, src: Image.Image, cx: float, cy: float) -> None:
    x = int(round(cx - src.size[0] / 2))
    y = int(round(cy - src.size[1] / 2))
    dst.alpha_composite(src, (x, y))


def render_arrow(width: int, height: int, color=BLUE) -> Image.Image:
    """A clear, slightly rounded arrow pointing at the Applications folder."""
    ss = 4
    W, H = width * ss, height * ss
    mask = Image.new("L", (W, H), 0)
    d = ImageDraw.Draw(mask)
    head_w = int(H * 0.72)
    shaft_h = int(H * 0.34)
    sy0 = (H - shaft_h) // 2
    sy1 = sy0 + shaft_h
    sx0 = int(0.04 * H)
    sx1 = W - head_w + int(0.20 * head_w)
    d.rounded_rectangle((sx0, sy0, sx1, sy1), radius=shaft_h / 2, fill=255)
    tip_x = W - max(1, int(0.03 * H))
    back_x = W - head_w
    d.polygon([(back_x, 0), (tip_x, H / 2), (back_x, H - 1)], fill=255)
    mask = mask.filter(ImageFilter.GaussianBlur(max(1, ss * 0.85)))
    mask = mask.point(lambda p: 255 if p >= 128 else 0)
    mask = mask.resize((width, height), Image.Resampling.LANCZOS)
    img = Image.new("RGBA", (width, height), color)
    img.putalpha(mask)
    return img


def render_background(scale: int = SCALE) -> Image.Image:
    w, h = WIN_W * scale, WIN_H * scale
    img = Image.new("RGBA", (w, h), BG)
    s = scale

    # Point from the app slot to the Applications folder.
    # Icon centers are (225, 229) and (525, 229), 128px icons; keep clear of both.
    arrow_w, arrow_h = int(118 * s), int(36 * s)
    mid_x = (APP_POS[0] + APPS_POS[0]) / 2 * s
    mid_y = APP_POS[1] * s
    paste_center(img, render_arrow(arrow_w, arrow_h, BLUE), mid_x, mid_y)

    # Quiet privacy note, safely above a possible 28px bottom clip
    text = "首次打开若被拦截，请到「系统设置 → 隐私与安全性」中允许打开"
    f = font(FONT_REG, int(13 * s))
    d = ImageDraw.Draw(img)
    bbox = d.textbbox((0, 0), text, font=f)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    tx = (w - tw) / 2 - bbox[0]
    ty = 452 * s - bbox[1]
    # slight hairline contrast without a card
    d.text((tx, ty), text, font=f, fill=NOTE)
    return img


def applications_icon(size: int) -> Image.Image:
    """Official macOS Applications folder icon, preview only (not bundled)."""
    im = Image.open(NATIVE_FOLDER).convert("RGBA")
    return im.resize((size, size), Image.Resampling.LANCZOS)


def load_app_icon(size: int) -> Image.Image:
    im = Image.open(ICON_PATH).convert("RGBA")
    return im.resize((size, size), Image.Resampling.LANCZOS)


def icon_shadow(size: int) -> Image.Image:
    # Finder-like soft contact shadow under the icon
    pad = int(size * 0.18)
    canvas = Image.new("RGBA", (size + pad * 2, size + pad * 2), (0, 0, 0, 0))
    d = ImageDraw.Draw(canvas)
    # ellipse under the icon
    ey0 = int(size * 0.90) + pad
    d.ellipse(
        (pad + int(size * 0.18), ey0, pad + size - int(size * 0.18), ey0 + int(size * 0.12)),
        fill=(0, 0, 0, 28),
    )
    return canvas.filter(ImageFilter.GaussianBlur(size * 0.06))


def render_preview(bg_1x: Image.Image) -> Image.Image:
    """macOS-style Finder window so the layout can be judged in place."""
    scale = 2
    tb = TITLEBAR * scale
    content_h = (WIN_H - TITLEBAR) * scale
    win_w, win_h = WIN_W * scale, WIN_H * scale
    # crop background to the visible content view (top-aligned, bottom clipped)
    bg = bg_1x.resize((win_w, win_h), Image.Resampling.LANCZOS)
    content = bg.crop((0, 0, win_w, content_h))

    # window on a page with drop shadow
    margin = 48
    page = Image.new("RGBA", (win_w + margin * 2, win_h + margin * 2 + 8), (236, 237, 240, 255))
    # window shadow
    shadow = Image.new("RGBA", page.size, (0, 0, 0, 0))
    sd = ImageDraw.Draw(shadow)
    sd.rounded_rectangle(
        (margin + 6, margin + 10, margin + win_w - 6, margin + win_h + 10),
        radius=16,
        fill=(0, 0, 0, 50),
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(14))
    page = Image.alpha_composite(page, shadow)

    window = Image.new("RGBA", (win_w, win_h), (0, 0, 0, 0))
    # title bar
    td = ImageDraw.Draw(window)
    td.rectangle((0, 0, win_w, tb), fill=(246, 246, 247, 255))
    window.paste(content, (0, tb))
    # hairline under title bar
    td.line((0, tb, win_w, tb), fill=(220, 220, 222, 255))

    # traffic lights
    colors = [(255, 95, 86), (255, 189, 46), (39, 201, 63)]
    r = int(6.5 * scale)
    gap = int(8 * scale)
    x = int(16 * scale)
    cy = tb // 2
    for i, col in enumerate(colors):
        cx = x + i * (2 * r + gap) + r
        td.ellipse((cx - r, cy - r, cx + r, cy + r), fill=col)

    # title
    tf = font(FONT_REG, int(13 * scale))
    title = "clipkeep"
    tbbox = td.textbbox((0, 0), title, font=tf)
    tw = tbbox[2] - tbbox[0]
    td.text(((win_w - tw) / 2, (tb - (tbbox[3] - tbbox[1])) / 2 - tbbox[1]), title, font=tf, fill=(60, 60, 62, 255))

    # icons in content coordinates (below title bar)
    ico = ICON_SIZE * scale
    app = load_app_icon(ico)
    folder = applications_icon(ico)
    app_shadow = icon_shadow(ico)

    def place_icon(icon, center, label, shadow=None):
        cx, cy = center[0] * scale, center[1] * scale
        if shadow is not None:
            paste_center(window, shadow, cx, cy + tb)
        paste_center(window, icon, cx, cy + tb)
        lf = font(FONT_REG, int(13 * scale))
        lb = ImageDraw.Draw(window)
        bb = lb.textbbox((0, 0), label, font=lf)
        lw = bb[2] - bb[0]
        # label just below the icon
        ly = cy + tb + ico / 2 + int(6 * scale) - bb[1]
        lx = cx - lw / 2 - bb[0]
        lb.text((lx, ly), label, font=lf, fill=LABEL)

    place_icon(app, APP_POS, "clipkeep", app_shadow)
    place_icon(folder, APPS_POS, "应用程序")

    # mask window to rounded rect
    mask = Image.new("L", (win_w, win_h), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, win_w - 1, win_h - 1), radius=int(10 * scale), fill=255)
    rounded = Image.new("RGBA", (win_w, win_h), (0, 0, 0, 0))
    rounded.paste(window, (0, 0))
    rounded.putalpha(mask)
    # keep pixels
    window = Image.composite(window, Image.new("RGBA", (win_w, win_h), (0, 0, 0, 0)), mask)

    page.alpha_composite(window, (margin, margin))
    return page


def save_tiff(im_1x: Image.Image, im_2x: Image.Image, path: Path) -> None:
    a = im_1x.convert("RGBA")
    b = im_2x.convert("RGBA")
    a.save(
        path,
        format="TIFF",
        save_all=True,
        append_images=[b],
        compression="tiff_lzw",
        dpi=(72, 72),
    )
    # Pillow may not keep 144 dpi on the second page; stamp it if possible
    # Re-open and rewrite via tifffile if needed — verify after save.


def main() -> None:
    ROOT.mkdir(parents=True, exist_ok=True)
    im2 = render_background(2)
    im1 = im2.resize((WIN_W, WIN_H), Image.Resampling.LANCZOS)
    im1.convert("RGBA").save(OUT_PNG, "PNG")
    save_tiff(im1, im2, OUT_TIFF)
    preview = render_preview(im1)
    preview.convert("RGBA").save(OUT_PREVIEW, "PNG")
    print(f"wrote {OUT_PNG} {im1.size}")
    print(f"wrote {OUT_TIFF}")
    print(f"wrote {OUT_PREVIEW} {preview.size}")


if __name__ == "__main__":
    main()
