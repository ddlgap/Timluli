from PIL import Image, ImageDraw, ImageFilter, ImageChops
import os

ICON_DIR = r"c:\Users\Lenovo\Desktop\speaky\src-tauri\icons"

def plasma_color(t):
    t = t % 1.0
    stops = [
        (0.00, (236, 72, 153)),
        (0.33, (139, 92, 246)),
        (0.66, (59, 130, 246)),
        (1.00, (236, 72, 153)),
    ]
    for i in range(len(stops) - 1):
        t0, c0 = stops[i]
        t1, c1 = stops[i + 1]
        if t0 <= t <= t1:
            f = (t - t0) / (t1 - t0)
            return tuple(int(c0[j] + (c1[j] - c0[j]) * f) for j in range(3))
    return stops[-1][1]

def make_icon(size):
    SCALE = 4
    s = size * SCALE
    cx = cy = s // 2
    r = s // 2 - SCALE * 2

    # --- Circle mask ---
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).ellipse([cx - r, cy - r, cx + r, cy + r], fill=255)

    # --- Layer 0: outer ring glow (subtle purple halo outside) ---
    ring_glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    rg = ImageDraw.Draw(ring_glow)
    rg.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(120, 70, 255, 60))
    ring_glow = ring_glow.filter(ImageFilter.GaussianBlur(radius=s // 10))

    # --- Layer 1: dark base circle ---
    base = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(base).ellipse(
        [cx - r, cy - r, cx + r, cy + r], fill=(7, 7, 18, 255)
    )

    # --- Layer 2: inner glow blob (behind mic, offset up slightly) ---
    blob = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    bd = ImageDraw.Draw(blob)
    gr = int(r * 0.58)
    gy = int(r * 0.06)
    # Purple core
    bd.ellipse(
        [cx - gr, cy - gr - gy, cx + gr, cy + gr - gy],
        fill=(80, 40, 210, 140),
    )
    blob = blob.filter(ImageFilter.GaussianBlur(radius=s // 8))
    # Clip blob to circle
    blob_a = ImageChops.multiply(blob.split()[3], mask)
    blob.putalpha(blob_a)

    # Subtle secondary blue glow
    blob2 = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    bd2 = ImageDraw.Draw(blob2)
    gr2 = int(r * 0.35)
    bd2.ellipse(
        [cx - gr2, cy - gr2 - gy, cx + gr2, cy + gr2 - gy],
        fill=(40, 80, 220, 110),
    )
    blob2 = blob2.filter(ImageFilter.GaussianBlur(radius=s // 12))
    blob2_a = ImageChops.multiply(blob2.split()[3], mask)
    blob2.putalpha(blob2_a)

    # --- Layer 3: plasma ring (thin, bright) ---
    ring = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    r_outer = r
    r_inner = int(r * 0.80)
    N = 720
    rd = ImageDraw.Draw(ring)
    for i in range(N):
        a_start = i * 360 / N - 90
        a_end = (i + 1.5) * 360 / N - 90
        col = plasma_color(i / N)
        rd.pieslice(
            [cx - r_outer, cy - r_outer, cx + r_outer, cy + r_outer],
            start=a_start,
            end=a_end,
            fill=(*col, 230),
        )
    # Cut inner hole
    ImageDraw.Draw(ring).ellipse(
        [cx - r_inner, cy - r_inner, cx + r_inner, cy + r_inner],
        fill=(0, 0, 0, 0),
    )
    # Add ring glow (blur copy)
    ring_blur = ring.copy()
    ring_blur = ring_blur.filter(ImageFilter.GaussianBlur(radius=s // 28))
    ring_blur_a = ImageChops.multiply(ring_blur.split()[3], mask)
    ring_blur.putalpha(ring_blur_a)

    # --- Layer 4: mic shape ---
    mic = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    md = ImageDraw.Draw(mic)
    mw = int(r * 0.20)
    mt = int(cy - r * 0.33)
    mb = int(cy + r * 0.06)
    lw = max(4, s // 22)

    # Capsule
    md.rounded_rectangle([cx - mw, mt, cx + mw, mb], radius=mw, fill=(255, 255, 255, 255))

    # Stand arc
    sr = int(r * 0.26)
    md.arc(
        [cx - sr, mb - sr, cx + sr, mb + sr],
        start=0, end=180,
        fill=(255, 255, 255, 255),
        width=lw,
    )

    # Stem
    st = mb + sr
    sb = st + int(r * 0.13)
    md.rectangle([cx - lw // 2, st, cx + lw // 2, sb], fill=(255, 255, 255, 255))

    # Base bar
    bw = int(r * 0.28)
    md.rectangle([cx - bw, sb, cx + bw, sb + lw], fill=(255, 255, 255, 255))

    # Mic white glow
    mic_glow = mic.copy()
    mic_glow = mic_glow.filter(ImageFilter.GaussianBlur(radius=s // 20))
    mic_glow_a = ImageChops.multiply(mic_glow.split()[3], mask)
    mic_glow.putalpha(mic_glow_a)

    # --- Composite everything ---
    out = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    out = Image.alpha_composite(out, ring_glow)   # outer halo
    out = Image.alpha_composite(out, base)         # dark circle
    out = Image.alpha_composite(out, blob)         # inner purple glow
    out = Image.alpha_composite(out, blob2)        # inner blue glow
    out = Image.alpha_composite(out, ring_blur)    # ring glow
    out = Image.alpha_composite(out, ring)         # sharp ring
    out = Image.alpha_composite(out, mic_glow)     # mic glow
    out = Image.alpha_composite(out, mic)          # sharp mic

    return out.resize((size, size), Image.LANCZOS)


print("Generating icons...")
os.makedirs(ICON_DIR, exist_ok=True)

icon_256 = make_icon(256)
icon_128 = make_icon(128)
icon_48  = make_icon(48)
icon_32  = make_icon(32)
icon_16  = make_icon(16)

# Save PNGs
icon_32.save(os.path.join(ICON_DIR, "32x32.png"))
icon_128.save(os.path.join(ICON_DIR, "128x128.png"))
icon_256.save(os.path.join(ICON_DIR, "128x128@2x.png"))
icon_256.save(os.path.join(ICON_DIR, "icon.png"))
print("PNGs saved.")

# Save ICO (multi-size)
icon_256.save(
    os.path.join(ICON_DIR, "icon.ico"),
    format="ICO",
    sizes=[(256, 256), (128, 128), (48, 48), (32, 32), (16, 16)],
    append_images=[icon_128, icon_48, icon_32, icon_16],
)
print("ICO saved.")
print("Done! Icons at:", ICON_DIR)
