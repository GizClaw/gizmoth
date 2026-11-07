import { useId } from "react";
import { CaretDown as ChevronDown, CaretUp as ChevronUp } from "@phosphor-icons/react";
import { GemEmitter } from "./GemEmitter";

/** Setting feedback only: these are not live display contents or sound measurements. */
export function DeviceControlEffect({ kind }: { kind: string }) {
  const id = useId();
  return (
    <div className={`control-effect-art effect-${kind}`} aria-hidden="true">
      <div className="control-output-cone">
        <span className="control-output-light" />
        {kind === "speaker" && (
          <svg className="control-sound-waves" viewBox="0 0 240 180" fill="none" preserveAspectRatio="none">
            <path className="wave-front wave-0" d="M96 143Q120 129 144 143" />
            <path className="wave-front wave-1" d="M71 101Q120 72 169 101" />
            <path className="wave-front wave-2" d="M36 52Q120 6 204 52" />
          </svg>
        )}
      </div>
      <span className="control-output-source" />
      {kind === "display" ? (
        <div className="control-screen">
          <span className="control-screen-glow" />
          <span className="control-screen-frame">
            <span className="control-screen-surface">
              <span className="control-screen-emission" />
              <span className="control-screen-reflection" />
            </span>
          </span>
        </div>
      ) : kind === "led" ? (
        <GemEmitter />
      ) : (
        <div className="control-sound-field">
          <span className="control-sound-field-glow" />
          <svg viewBox="0 0 160 80" fill="none">
            <defs>
              <radialGradient id={`${id}-field`}>
                <stop stopColor="#dcf3ff" />
                <stop offset=".4" stopColor="#8cc9ff" stopOpacity=".6" />
                <stop offset="1" stopColor="#77bfff" stopOpacity="0" />
              </radialGradient>
              <linearGradient id={`${id}-field-depth`} x1="0" y1="0" x2="0" y2="1">
                <stop stopColor="#5a809e" />
                <stop offset="1" stopColor="#091321" />
              </linearGradient>
              <radialGradient id={`${id}-field-surface`} cx="50%" cy="20%" r="80%">
                <stop stopColor="#2c4c69" />
                <stop offset="1" stopColor="#0e1b2d" />
              </radialGradient>
            </defs>
            <ellipse cx="80" cy="48" rx="64" ry="28" fill="#060c16" />
            <path d="M16 40V47A64 28 0 0 0 144 47V40A64 28 0 0 1 16 40Z" fill={`url(#${id}-field-depth)`} />
            <ellipse
              className="control-sound-field-bed"
              cx="80"
              cy="40"
              rx="64"
              ry="28"
              fill={`url(#${id}-field-surface)`}
            />
            <g className="control-sound-field-lines">
              <ellipse cx="80" cy="40" rx="62" ry="26" />
              <ellipse cx="80" cy="40" rx="46" ry="19" />
              <ellipse cx="80" cy="40" rx="29" ry="12" />
            </g>
            <ellipse className="control-sound-field-core" cx="80" cy="40" rx="24" ry="14" fill={`url(#${id}-field)`} />
          </svg>
        </div>
      )}
      <span className="control-drag-direction">
        <ChevronUp size={14} />
        <span className="control-drag-track">
          <span />
        </span>
        <ChevronDown size={14} />
      </span>
    </div>
  );
}
