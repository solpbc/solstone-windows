// The caption color is the sun-arc frame at the top edge of the window.
// The engine paints again every 60 seconds while the window is visible, on
// resize, when the appearance changes, and when the window becomes visible.
// This reports that paint. A paint that leaves the color unchanged does not
// call the window again.

export type Rgb = { red: number; green: number; blue: number };

export type GlowSample = {
  x: number;
  y: number;
  r: number;
  a: number;
  color: string;
};

/** Ground, with any glow that reaches the top center composited over it. */
export function compositeTopEdge(
  ground: string,
  glows: Array<GlowSample | null | undefined>,
  width: number,
  glowAt: (glow: GlowSample, x: number, y: number, tokens: unknown) => number,
  parse: (color: string) => number[],
  tokens: unknown,
): Rgb {
  const parsed = parse(ground);
  let red = parsed[0] ?? 0;
  let green = parsed[1] ?? 0;
  let blue = parsed[2] ?? 0;
  const x = width / 2;
  for (const glow of glows) {
    if (!glow) continue;
    const alpha = Math.max(0, Math.min(1, glowAt(glow, x, 0, tokens)));
    if (alpha <= 0) continue;
    const over = parse(glow.color);
    red = red * (1 - alpha) + (over[0] ?? 0) * alpha;
    green = green * (1 - alpha) + (over[1] ?? 0) * alpha;
    blue = blue * (1 - alpha) + (over[2] ?? 0) * alpha;
  }
  return { red: Math.round(red), green: Math.round(green), blue: Math.round(blue) };
}

type FrameLike = {
  ground?: string;
  glow?: GlowSample | null;
  halo?: GlowSample | null;
};

type SunArcLike = {
  parseTokens?: (root: HTMLElement) => unknown;
  tokensValid?: (tokens: unknown) => boolean;
  glowAt?: (glow: GlowSample, x: number, y: number, tokens: unknown) => number;
  parseColorToRgb?: (color: string) => number[];
};

let lastSent = "";

/** Read the mounted frame and report a new top-edge color. */
export function publishCaptionColor(
  ground: HTMLElement,
  frame: FrameLike | null,
  send: (color: Rgb) => void,
): void {
  if (!frame?.ground) return;
  const engine = (window as unknown as { SunArc?: SunArcLike }).SunArc;
  const tokens = engine?.parseTokens?.(ground);
  const valid = tokens && engine?.tokensValid?.(tokens) && engine.glowAt && engine.parseColorToRgb;
  const color = valid
    ? compositeTopEdge(
        frame.ground,
        [frame.glow, frame.halo],
        ground.clientWidth || window.innerWidth || 1,
        engine.glowAt as (glow: GlowSample, x: number, y: number, tokens: unknown) => number,
        engine.parseColorToRgb as (color: string) => number[],
        tokens,
      )
    : hexColor(frame.ground);
  if (!color) return;
  const key = `${color.red},${color.green},${color.blue}`;
  if (key === lastSent) return;
  lastSent = key;
  send(color);
}

export function resetCaptionReport(): void {
  lastSent = "";
}

function hexColor(value: string): Rgb | null {
  const match = /^#([0-9a-f]{6})$/i.exec(value.trim());
  if (!match) return null;
  const hex = match[1];
  return {
    red: Number.parseInt(hex.slice(0, 2), 16),
    green: Number.parseInt(hex.slice(2, 4), 16),
    blue: Number.parseInt(hex.slice(4, 6), 16),
  };
}
