import { t, useI18n } from "../i18n";
import { useEffect, useId, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { CaretDown, CaretRight, Check } from "@phosphor-icons/react";
import { Button, SearchInput } from "../ui";
import type { Provider } from "../bridge";
import { modelRef } from "../providers";
import { formatTokens, thinkingLabels, type Effort } from "../agent-thinking";
import "./AgentModelPicker.css";

const SUBMENU_WIDTH = 176;
type Side = "right" | "left" | "over";

/**
 * One trigger for everything the user can change about the assistant: the model
 * list (two-line rows) and a separated thinking-effort row with a submenu.
 */
export function AgentModelPicker({
  providers,
  value,
  disabled,
  onChange,
  thinking,
  thinkingLevels,
  onThinkingChange,
}: {
  providers: Provider[];
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
  thinking: Effort;
  thinkingLevels: Effort[];
  onThinkingChange: (value: Effort) => void;
}) {
  useI18n();
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const popover = useRef<HTMLDivElement>(null);
  const effortButton = useRef<HTMLButtonElement>(null);
  const submenu = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(value);
  const [position, setPosition] = useState<CSSProperties>({});
  const [effortOpen, setEffortOpen] = useState(false);
  const [side, setSide] = useState<Side>("right");
  const options = useMemo(
    () =>
      providers.flatMap((provider) =>
        provider.models.map((model) => ({
          value: modelRef(provider, model),
          name: model.name,
          modelId: model.id,
          provider: provider.name,
          contextWindow: model.contextWindow,
          reasoning: model.reasoning,
        })),
      ),
    [providers],
  );
  const selected = options.find((option) => option.value === value);
  const search = query.trim().toLocaleLowerCase();
  const filtered = options.filter((option) =>
    `${option.name} ${option.modelId} ${option.provider}`.toLocaleLowerCase().includes(search),
  );
  const activeIndex = Math.max(
    0,
    filtered.findIndex((option) => option.value === active),
  );
  const activeId = filtered.length ? `${id}-option-${activeIndex}` : undefined;
  const adjustable = thinkingLevels.length > 1;
  const effortLabel = thinking === "off" ? t("关闭思考") : t(thinkingLabels[thinking]);
  const triggerName = selected?.name ?? (options.length ? t("选择模型") : t("未配置模型"));

  function close(restoreFocus = true) {
    setEffortOpen(false);
    popover.current?.hidePopover();
    if (restoreFocus) trigger.current?.focus();
  }

  function show() {
    if (disabled || !trigger.current) return;
    const rect = trigger.current.getBoundingClientRect();
    const above = rect.top - 20;
    const below = window.innerHeight - rect.bottom - 20;
    const upwards = above >= 180 || above >= below;
    const width = Math.min(312, window.innerWidth - 24);
    // Anchor the menu's right edge to the trigger: the trigger sits at the composer's right.
    const left = Math.max(12, Math.min(rect.right - width, window.innerWidth - width - 12));
    setSide(
      left + width + 4 + SUBMENU_WIDTH <= window.innerWidth - 8
        ? "right"
        : left - 4 - SUBMENU_WIDTH >= 8
          ? "left"
          : "over",
    );
    setPosition({
      left,
      ...(upwards ? { bottom: window.innerHeight - rect.top + 8 } : { top: rect.bottom + 8 }),
      maxHeight: Math.min(420, Math.max(0, upwards ? above : below)),
    });
    setQuery("");
    setActive(value);
    setEffortOpen(false);
    popover.current?.showPopover();
  }

  useEffect(() => {
    if (!open) return;
    popover.current?.querySelector<HTMLInputElement>("input")?.focus();
    const dismiss = () => close(false);
    window.addEventListener("resize", dismiss);
    return () => window.removeEventListener("resize", dismiss);
  }, [open]);

  useEffect(() => {
    if (open && disabled) close(false);
  }, [disabled, open]);

  useEffect(() => {
    if (open && activeId) document.getElementById(activeId)?.scrollIntoView({ block: "nearest" });
  }, [open, activeId]);

  function choose(next: string) {
    if (disabled) return;
    onChange(next);
    close();
  }

  function chooseEffort(next: Effort) {
    if (disabled || !thinkingLevels.includes(next)) return;
    onThinkingChange(next);
    close();
  }

  function openEffort(focusItem: boolean) {
    if (!adjustable) return;
    setEffortOpen(true);
    if (focusItem)
      requestAnimationFrame(() => {
        const menu = submenu.current;
        (
          menu?.querySelector<HTMLElement>('[aria-checked="true"]') ??
          menu?.querySelector<HTMLElement>('[role="menuitemradio"]')
        )?.focus();
      });
  }

  function closeEffort() {
    setEffortOpen(false);
    effortButton.current?.focus();
  }

  function navigate(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    let index = activeIndex;
    if (event.key === "ArrowDown") index = (activeIndex + 1) % filtered.length;
    else if (event.key === "ArrowUp") index = (activeIndex - 1 + filtered.length) % filtered.length;
    else if (event.key === "Home") index = 0;
    else if (event.key === "End") index = filtered.length - 1;
    else if (event.key === "Enter") {
      event.preventDefault();
      if (filtered[activeIndex]) choose(filtered[activeIndex].value);
      return;
    } else return;
    event.preventDefault();
    setEffortOpen(false);
    if (filtered[index]) setActive(filtered[index].value);
  }

  function moveInSubmenu(event: KeyboardEvent<HTMLDivElement>) {
    const items = [...(submenu.current?.querySelectorAll<HTMLElement>('[role="menuitemradio"]') ?? [])];
    const current = items.indexOf(document.activeElement as HTMLElement);
    let next = current;
    if (event.key === "ArrowDown") next = (current + 1) % items.length;
    else if (event.key === "ArrowUp") next = (current - 1 + items.length) % items.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = items.length - 1;
    else if (event.key === "ArrowLeft" || event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeEffort();
      return;
    } else return;
    event.preventDefault();
    items[next]?.focus();
  }

  return (
    <>
      <Button
        ref={trigger}
        controlSize="sm"
        variant="ghost"
        className="agent-model-trigger"
        aria-label={
          adjustable
            ? t("选择模型：{name}，思考程度{effort}", { name: triggerName, effort: effortLabel })
            : t("选择模型：{name}", { name: triggerName })
        }
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={`${id}-popover`}
        data-thinking={thinking}
        title={selected ? `${selected.provider} / ${selected.modelId}` : t("未配置模型")}
        disabled={disabled}
        onClick={() => (open ? close() : show())}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            show();
          }
        }}
      >
        <span className="agent-model-trigger-name">{triggerName}</span>
        {adjustable && (
          <span className="agent-model-trigger-effort">{t("思考 {effort}", { effort: effortLabel })}</span>
        )}
        <CaretDown size={12} aria-hidden="true" />
      </Button>
      <div
        ref={popover}
        id={`${id}-popover`}
        popover="auto"
        role="dialog"
        aria-label={t("模型与思考程度")}
        className="agent-model-popover"
        data-submenu-side={side}
        style={position}
        onToggle={(event) => {
          setOpen(event.newState === "open");
          if (event.newState !== "open") setEffortOpen(false);
        }}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.keyCode === 229) return;
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            close();
          } else if (event.key === "Tab") {
            // Let focus move inside the menu; close once it leaves.
            requestAnimationFrame(() => {
              if (!popover.current?.contains(document.activeElement)) close(false);
            });
          }
        }}
      >
        <SearchInput
          autoComplete="off"
          aria-label={t("搜索模型")}
          placeholder={t("搜索模型…")}
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={open}
          aria-controls={`${id}-list`}
          aria-activedescendant={activeId}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setActive("");
          }}
          onClear={() => {
            setQuery("");
            setActive(value);
          }}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing || event.keyCode === 229) return;
            // Escape dismisses the entire chooser even when a search is present.
            if (event.key === "Escape") {
              event.preventDefault();
              close();
            } else navigate(event);
          }}
        />
        <div className="agent-model-options" id={`${id}-list`} role="listbox" aria-label={t("模型")}>
          {filtered.map((option, index) => (
            <div
              key={option.value}
              id={`${id}-option-${index}`}
              role="option"
              aria-selected={option.value === value}
              className={`agent-model-option${index === activeIndex ? " is-active" : ""}`}
              title={`${option.name}\n${option.modelId}`}
              onPointerMove={() => {
                setActive(option.value);
                setEffortOpen(false);
              }}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(option.value)}
            >
              <span className="agent-model-option-text">
                <span className="agent-model-option-name">
                  {option.name}
                  <span className="agent-model-option-provider"> · {option.provider}</span>
                </span>
                <span className="agent-model-option-meta">
                  {option.contextWindow > 0
                    ? t("{count} 上下文", { count: formatTokens(option.contextWindow) })
                    : t("上下文未知")}
                  {option.reasoning ? t(" · 支持思考") : ""}
                </span>
              </span>
              {option.value === value && <Check size={14} aria-hidden="true" />}
            </div>
          ))}
          {!filtered.length && (
            <p className="agent-model-empty" role="status">
              {t("没有匹配的模型，试试其他关键词。")}
            </p>
          )}
        </div>
        <div className="agent-model-effort">
          <button
            ref={effortButton}
            type="button"
            className={`agent-model-effort-row${effortOpen ? " is-open" : ""}`}
            aria-label={t("思考程度：{effort}", { effort: adjustable ? effortLabel : t("当前模型不支持") })}
            aria-haspopup="menu"
            aria-expanded={effortOpen}
            aria-controls={`${id}-effort`}
            disabled={disabled || !adjustable}
            title={adjustable ? t("调整当前模型的思考程度") : t("当前模型未声明支持思考程度调整")}
            onPointerEnter={() => openEffort(false)}
            // Hover already opened it; a click only moves focus in, it never toggles it shut.
            onClick={() => openEffort(true)}
            onKeyDown={(event) => {
              if (event.key === "ArrowRight" || event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                openEffort(true);
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                popover.current?.querySelector<HTMLInputElement>("input")?.focus();
              }
            }}
          >
            <span>{t("思考程度")}</span>
            <span className="agent-model-effort-value">{adjustable ? effortLabel : t("不支持")}</span>
            <CaretRight size={12} aria-hidden="true" />
          </button>
          <div
            ref={submenu}
            id={`${id}-effort`}
            role="menu"
            aria-label={t("思考程度")}
            className="agent-model-effort-menu"
            hidden={!effortOpen}
            onKeyDown={moveInSubmenu}
          >
            {thinkingLevels.map((level) => (
              <button
                key={level}
                type="button"
                role="menuitemradio"
                aria-checked={level === thinking}
                data-value={level}
                tabIndex={level === thinking ? 0 : -1}
                className="agent-model-effort-item"
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => chooseEffort(level)}
              >
                <span>{level === "off" ? t("关闭思考") : t(thinkingLabels[level])}</span>
                {level === thinking && <Check size={14} aria-hidden="true" />}
              </button>
            ))}
          </div>
        </div>
      </div>
    </>
  );
}
