# Generates ZAI_MARK_ALPHA in src/BetterTrafficMonitorAiUsage/ProviderMarkAssets.h
# (a bold Z glyph; z.ai publishes no freely licensed mark to rasterize).
# Usage: python3 assets/brands/zai-z-glyph.py


SIZE = 32
SS = 8

# Z shape: top bar, diagonal (top-right -> bottom-left), bottom bar.
# (x0, y0, x1, y1) rectangles in 32px space; diagonal is a quad.
top_bar = (5, 5, 27, 11)
bottom_bar = (5, 21, 27, 27)
diagonal_quad = ((13, 11), (27, 11), (19, 21), (5, 21))  # top edge -> bottom edge


def inside_rect(x, y, r):
    return r[0] <= x < r[2] and r[1] <= y < r[3]


def sign(ax, ay, bx, by, px, py):
    return (px - ax) * (by - ay) - (py - ay) * (bx - ax)


def inside_quad(x, y, quad):
    a, b, c, d = quad
    s1 = sign(a[0], a[1], b[0], b[1], x, y)
    s2 = sign(b[0], b[1], c[0], c[1], x, y)
    s3 = sign(c[0], c[1], d[0], d[1], x, y)
    s4 = sign(d[0], d[1], a[0], a[1], x, y)
    pos = s1 >= 0 and s2 >= 0 and s3 >= 0 and s4 >= 0
    neg = s1 <= 0 and s2 <= 0 and s3 <= 0 and s4 <= 0
    return pos or neg


rows = []
for py in range(SIZE):
    row = []
    for px in range(SIZE):
        covered = 0
        for sy in range(SS):
            for sx in range(SS):
                x = px + (sx + 0.5) / SS
                y = py + (sy + 0.5) / SS
                if inside_rect(x, y, top_bar) or inside_rect(x, y, bottom_bar) or inside_quad(x, y, diagonal_quad):
                    covered += 1
        alpha = round(255 * covered / (SS * SS))
        row.append(alpha)
    rows.append(row)

lines = []
for row in rows:
    lines.append("    " + ", ".join(str(v) for v in row) + ",")
body = "\n".join(lines)

print("inline constexpr std::uint8_t ZAI_MARK_ALPHA[PROVIDER_MARK_SIZE * PROVIDER_MARK_SIZE] = {")
print(body)
print("};")
