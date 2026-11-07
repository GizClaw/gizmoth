import "./GemEmitter.css";

const radius = 56;
const elevation = (25 * Math.PI) / 180;
const latitudeCount = 7;
const longitudeCount = 24;
type Point = { x: number; y: number; z: number };

function point(latitude: number, longitude: number): Point {
  const theta = ((latitude / latitudeCount) * Math.PI) / 2;
  const phi = (longitude / longitudeCount) * Math.PI * 2;
  return {
    x: radius * Math.sin(theta) * Math.cos(phi),
    y: radius * Math.sin(theta) * Math.sin(phi),
    z: radius * Math.cos(theta),
  };
}

function triangle(points: Point[]) {
  const center = points.reduce((a, p) => ({ x: a.x + p.x / 3, y: a.y + p.y / 3, z: a.z + p.z / 3 }), {
    x: 0,
    y: 0,
    z: 0,
  });
  const length = Math.hypot(center.x, center.y, center.z);
  const light = Math.max(0, (-0.48 * center.x - 0.28 * center.y + 0.83 * center.z) / length);
  const shade = Math.min(1, 0.27 + light * 0.73);
  const dark = [24, 48, 65].map((v, i) => Math.round(v + shade * [47, 54, 58][i]));
  const lit = [53, 104, 136].map((v, i) => Math.round(v + shade * [126, 112, 99][i]));
  return {
    points: points
      .map((p) => `${(60 + p.x).toFixed(2)},${(60 + p.y * Math.sin(elevation) - p.z * Math.cos(elevation)).toFixed(2)}`)
      .join(" "),
    depth: center.y * Math.cos(elevation) + center.z * Math.sin(elevation),
    fill: `color-mix(in srgb, rgb(${dark.join(" ")}), rgb(${lit.join(" ")}) var(--gem-emitter-level))`,
  };
}

// Fixed geometry: lighting and brightness do not move the emitter or its facets.
const facets = Array.from({ length: latitudeCount }, (_, row) =>
  Array.from({ length: longitudeCount }, (_, column) => {
    const a = point(row, column);
    const b = point(row + 1, column);
    const c = point(row + 1, column + 1);
    const d = point(row, column + 1);
    return row === 0 ? [triangle([a, b, c])] : [triangle([a, b, c]), triangle([a, c, d])];
  }).flat(),
)
  .flat()
  .sort((a, b) => a.depth - b.depth);

/** Radius-56 hemisphere viewed 25 degrees above a horizontal circular base. */
export function GemEmitter() {
  return (
    <div className="gem-emitter" aria-hidden="true">
      <span className="gem-emitter-spill" />
      <svg className="gem-emitter-solid" viewBox="0 0 120 90" fill="none">
        <ellipse className="gem-emitter-base" cx="60" cy="61" rx="56" ry="23.67" />
        <g className="gem-emitter-mesh">
          {facets.map((facet, index) => (
            <polygon key={index} points={facet.points} fill={facet.fill} stroke={facet.fill} strokeWidth="0.35" />
          ))}
        </g>
        <path className="gem-emitter-edge-low" d="M4 60A56 23.67 0 0 0 116 60" />
      </svg>
    </div>
  );
}
