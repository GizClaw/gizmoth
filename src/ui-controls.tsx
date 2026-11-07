import { useRef, type ComponentPropsWithRef, type ComponentPropsWithoutRef } from "react";
import { MagnifyingGlass, X } from "@phosphor-icons/react";
import "./ui-controls.css";
import { useI18n } from "./i18n";

export type ControlSize = "sm" | "md" | "lg";
type ControlOptions = { controlSize?: ControlSize };
const classes = (...values: (string | undefined | false)[]) => values.filter(Boolean).join(" ");

export type ButtonProps = ComponentPropsWithRef<"button"> &
  ControlOptions & {
    variant?: "default" | "primary" | "danger" | "ghost";
  };

/** Buttons are non-submitting by default; form submit actions must opt in. */
export function Button({ variant = "default", controlSize = "md", className, type = "button", ...props }: ButtonProps) {
  return (
    <button
      {...props}
      type={type}
      className={classes("button", `control-${controlSize}`, variant !== "default" && variant, className)}
    />
  );
}

type IconLabel = { "aria-label": string; title?: string } | { title: string; "aria-label"?: string };
export type IconButtonProps = ComponentPropsWithRef<"button"> & ControlOptions & IconLabel;
export function IconButton({
  controlSize = "sm",
  className,
  type = "button",
  title,
  "aria-label": label,
  ...props
}: IconButtonProps) {
  return (
    <button
      {...props}
      type={type}
      title={title}
      aria-label={label ?? title}
      className={classes("icon-button", `control-${controlSize}`, className)}
    />
  );
}

export type InputProps = ComponentPropsWithRef<"input"> & ControlOptions;
export function Input({ controlSize = "md", className, ...props }: InputProps) {
  return <input {...props} className={classes("ui-input", `control-${controlSize}`, className)} />;
}

export { Select } from "./Select";
export type { SelectProps, SelectOption } from "./Select";

export type SearchInputProps = Omit<ComponentPropsWithoutRef<"input">, "type" | "size" | "value"> &
  ControlOptions & {
    value: string;
    onClear: () => void;
    clearLabel?: string;
  };

/** Search keeps its leading icon, clear target and Escape behavior consistent across pages. */
export function SearchInput({
  controlSize = "md",
  className,
  style,
  value,
  onClear,
  clearLabel,
  onKeyDown,
  disabled,
  ...props
}: SearchInputProps) {
  const { t } = useI18n();
  const input = useRef<HTMLInputElement>(null);
  const clear = () => {
    onClear();
    input.current?.focus();
  };
  return (
    <span className={classes("ui-search", `control-${controlSize}`, className)} style={style}>
      <MagnifyingGlass className="ui-search-leading" size={14} aria-hidden="true" />
      <input
        {...props}
        ref={input}
        type="search"
        className="ui-input"
        value={value}
        disabled={disabled}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.keyCode === 229) return;
          onKeyDown?.(event);
          if (!event.defaultPrevented && event.key === "Escape" && value) {
            event.preventDefault();
            event.stopPropagation();
            clear();
          }
        }}
      />
      {value && (
        <IconButton
          className="ui-search-clear"
          aria-label={clearLabel ?? t("清除搜索")}
          title={clearLabel ?? t("清除搜索")}
          disabled={disabled}
          onClick={clear}
        >
          <X size={12} />
        </IconButton>
      )}
    </span>
  );
}
