import { Component } from "solid-js";

const SEAL_PALETTE = {
  dark: {
    bg: "#0E1013",
    rule: "#2B313B",
    starF: "#A8AEBA",
    starA: "#C9A86A",
    starP: "#E8E4D6",
    planet: "#D9C28A",
    planetRim: "#3A3220",
    orbit: "#3A4150",
    label: "#A8AEBA",
    labelDim: "#4A505C",
    triLine: "#2B313B",
  },
  light: {
    bg: "#F2EEE5",
    rule: "#C0B8A8",
    starF: "#5A6070",
    starA: "#8A6A30",
    starP: "#1A1812",
    planet: "#8A6A30",
    planetRim: "#5A4220",
    orbit: "#C0B8A8",
    label: "#3A3528",
    labelDim: "#9A9080",
    triLine: "#C0B8A8",
  },
} as const;

const ProximaSeal: Component<{
  size?: number;
  theme?: "dark" | "light";
  mode?: "full" | "mark" | "favicon";
  showLabels?: boolean;
}> = (props) => {
  const p = SEAL_PALETTE[props.theme ?? "dark"];
  const cx = 50;
  const cy = 50;
  const R = 34;
  const size = props.size ?? 320;
  const P_v = { x: cx, y: cy - R };
  const F_v = { x: cx - R * Math.sin(Math.PI / 3), y: cy + R * Math.cos(Math.PI / 3) };
  const A_v = { x: cx + R * Math.sin(Math.PI / 3), y: cy + R * Math.cos(Math.PI / 3) };

  const isFavicon = props.mode === "favicon";
  const wantOrbit = !isFavicon;
  const wantLabels = (props.showLabels ?? true) && props.mode === "full";
  const wantTri = !isFavicon;
  const wantBinaryArc = !isFavicon;

  return (
    <svg
      viewBox="0 0 100 100"
      width={size}
      height={size}
      xmlns="http://www.w3.org/2000/svg"
      style={{ display: "block" }}
    >
      <circle cx={cx} cy={cy} r={48} fill={p.bg} />
      <circle cx={cx} cy={cy} r={47.4} fill="none" stroke={p.rule} stroke-width="0.4" />

      {wantOrbit && (
        <g opacity="0.6">
          <circle cx={cx} cy={cy} r={R} fill="none" stroke={p.triLine} stroke-width="0.25" stroke-dasharray="0.6 1.2" />
          <ellipse cx={cx} cy={cy} rx="11" ry="3.2" fill="none" stroke={p.orbit} stroke-width="0.35" />
          <ellipse cx={cx} cy={cy} rx="3.2" ry="11" fill="none" stroke={p.orbit} stroke-width="0.35" opacity="0.6" />
        </g>
      )}

      {wantTri && (
        <g opacity="0.35">
          <line x1={P_v.x} y1={P_v.y} x2={F_v.x} y2={F_v.y} stroke={p.triLine} stroke-width="0.3" stroke-dasharray="0.4 0.8" />
          <line x1={P_v.x} y1={P_v.y} x2={A_v.x} y2={A_v.y} stroke={p.triLine} stroke-width="0.3" stroke-dasharray="0.4 0.8" />
          <line x1={F_v.x} y1={F_v.y} x2={A_v.x} y2={A_v.y} stroke={p.triLine} stroke-width="0.3" />
        </g>
      )}

      {wantBinaryArc && (
        <path
          d={`M ${F_v.x} ${F_v.y} Q ${cx} ${cy + R + 4} ${A_v.x} ${A_v.y}`}
          fill="none"
          stroke={p.starA}
          stroke-width="0.4"
          opacity="0.45"
        />
      )}

      <g>
        {!isFavicon && <ellipse cx={cx} cy={cy} rx="9.5" ry="2.2" fill="none" stroke={p.planetRim} stroke-width="0.7" />}
        <circle cx={cx} cy={cy} r={isFavicon ? 6 : 4.2} fill={p.planet} />
        {!isFavicon && (
          <path
            d={`M ${cx - 3.6} ${cy - 1.6} a 4.2 4.2 0 0 0 6.8 2.4 a 3.2 3.2 0 0 1 -6.8 -2.4 z`}
            fill={p.planetRim}
            opacity="0.6"
          />
        )}
        {!isFavicon && (
          <path
            d={`M ${cx - 9.5} ${cy} a 9.5 2.2 0 0 0 19 0`}
            fill="none"
            stroke={p.planetRim}
            stroke-width="0.7"
          />
        )}
      </g>

      <g>
        <circle cx={P_v.x} cy={P_v.y} r={isFavicon ? 3 : 2.6} fill={p.starP} />
        {!isFavicon && (
          <>
            <line x1={P_v.x} y1={P_v.y - 4.2} x2={P_v.x} y2={P_v.y - 5.6} stroke={p.starP} stroke-width="0.4" />
            <line x1={P_v.x} y1={P_v.y + 4.2} x2={P_v.x} y2={P_v.y + 5.6} stroke={p.starP} stroke-width="0.4" />
            <line x1={P_v.x - 4.2} y1={P_v.y} x2={P_v.x - 5.6} y2={P_v.y} stroke={p.starP} stroke-width="0.4" />
            <line x1={P_v.x + 4.2} y1={P_v.y} x2={P_v.x + 5.6} y2={P_v.y} stroke={p.starP} stroke-width="0.4" />
          </>
        )}
      </g>

      <circle cx={F_v.x} cy={F_v.y} r={isFavicon ? 2.4 : 2.0} fill={p.starF} />
      <circle cx={A_v.x} cy={A_v.y} r={isFavicon ? 2.4 : 2.0} fill={p.starA} />

      {wantLabels && (
        <g font-family="JetBrains Mono, ui-monospace, monospace" font-size="3.6" fill={p.label} letter-spacing="0.3">
          <text x={P_v.x} y={P_v.y - 7.5} text-anchor="middle">
            P
          </text>
          <text x={F_v.x - 4.5} y={F_v.y + 4.5} text-anchor="end">
            F
          </text>
          <text x={A_v.x + 4.5} y={A_v.y + 4.5} text-anchor="start">
            A
          </text>
          <text x={cx} y={cy + 8.2} text-anchor="middle" fill={p.labelDim} font-size="2.6">
            G
          </text>
        </g>
      )}

      {props.mode === "full" && (
        <g font-family="Newsreader, Georgia, serif" fill={p.label}>
          <text x={cx} y={92} text-anchor="middle" font-size="5.2" font-style="italic" letter-spacing="0.4">
            proxima
          </text>
        </g>
      )}
    </svg>
  );
};

export const SealAssets: Component = () => (
  <div class="proxima-seal-assets">
    <ProximaSeal size={420} theme="dark" mode="full" />
  </div>
);
