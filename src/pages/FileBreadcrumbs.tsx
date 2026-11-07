import { t, useI18n } from "../i18n";
import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
} from "react";
import { createPortal } from "react-dom";
import { CaretDown, CaretRight, FileCode, Folder, FolderOpen } from "@phosphor-icons/react";
import {
  projectFileTree,
  projectPathSegments,
  projectTreeChildren,
  visibleProjectTree,
  type ProjectFile,
} from "../project-files.ts";
import { matchSelectValue, selectPopupPosition } from "../select-options.ts";
import "./FileBreadcrumbs.css";

/** Project-scoped breadcrumb navigation with expandable sibling folders and files. */
export function FileBreadcrumbs({
  files,
  path,
  disabled,
  onOpen,
}: {
  files: ProjectFile[];
  path: string;
  disabled: boolean;
  onOpen: (path: string) => Promise<boolean>;
}) {
  useI18n();
  const id = useId();
  const nav = useRef<HTMLElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const [container, setContainer] = useState<HTMLElement | null>(null);
  const [open, setOpen] = useState(false);
  const [context, setContext] = useState({ parent: "", index: -1 });
  const [active, setActive] = useState("");
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const [position, setPosition] = useState<CSSProperties>({});
  const query = useRef({ text: "", at: 0 });
  const alive = useRef(true);
  const nativePopover = typeof HTMLElement !== "undefined" && typeof HTMLElement.prototype.showPopover === "function";
  const segments = projectPathSegments(path);
  const tree = useMemo(() => projectFileTree(files), [files]);
  const siblings = useMemo(() => projectTreeChildren(tree, context.parent), [tree, context.parent]);
  const rows = useMemo(() => visibleProjectTree(siblings, expanded), [siblings, expanded]);
  const activeIndex = rows.findIndex(({ node }) => node.path === active);
  const popupId = `${id}-files`;
  const unavailable = disabled || files.length === 0;

  useLayoutEffect(() => {
    setContainer(nav.current?.closest<HTMLElement>("dialog, [popover]") ?? document.body);
  }, []);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    nav.current?.querySelector('[aria-current="page"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });
    close();
  }, [path]);
  useEffect(() => {
    if (unavailable) close();
  }, [unavailable]);
  useEffect(() => {
    if (open && activeIndex < 0) setActive(rows[0]?.node.path ?? "");
  }, [open, rows, activeIndex]);
  useLayoutEffect(() => {
    if (open && activeIndex >= 0)
      popup.current?.querySelector(`[data-row-index="${activeIndex}"]`)?.scrollIntoView({ block: "nearest" });
  }, [open, activeIndex]);
  useLayoutEffect(() => {
    // Fallback menus become focusable only after React makes them visible.
    if (open) popup.current?.focus({ preventScroll: true });
  }, [open, context]);

  function close(restore = false) {
    if (nativePopover && popup.current?.isConnected && popup.current.matches(":popover-open"))
      popup.current.hidePopover();
    setOpen(false);
    query.current = { text: "", at: 0 };
    if (restore) {
      const target = trigger.current?.isConnected
        ? trigger.current
        : nav.current?.querySelector<HTMLButtonElement>('[aria-current="page"]');
      target?.focus({ preventScroll: true });
    }
  }
  function show(button: HTMLButtonElement, parent: string, selected: string, index: number) {
    if (unavailable || !popup.current) return;
    if (open && trigger.current === button) {
      close(true);
      return;
    }
    trigger.current = button;
    button.focus({ preventScroll: true });
    setContext({ parent, index });
    query.current = { text: "", at: 0 };
    setExpanded(new Set());
    setActive(selected);
    const count = projectTreeChildren(tree, parent).length;
    setPosition(
      selectPopupPosition(button.getBoundingClientRect(), { width: innerWidth, height: innerHeight }, count, 240),
    );
    setOpen(true);
    if (nativePopover) popup.current.showPopover();
    popup.current.focus({ preventScroll: true });
  }
  function toggle(folder: string, force?: boolean) {
    setExpanded((previous) => {
      const next = new Set(previous);
      if (force ?? !previous.has(folder)) next.add(folder);
      else next.delete(folder);
      return next;
    });
  }
  async function choose(file: string) {
    if (unavailable) return;
    const previousTrigger = trigger.current;
    close(true);
    const ok = await onOpen(file);
    if (!ok || !alive.current) return;
    requestAnimationFrame(() => {
      if (!alive.current) return;
      const focused = document.activeElement;
      if (focused === document.body || focused === previousTrigger)
        nav.current?.querySelector<HTMLButtonElement>('[aria-current="page"]')?.focus({ preventScroll: true });
    });
  }
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!popup.current?.contains(target) && !nav.current?.contains(target)) close();
    };
    const scroll = (event: Event) => {
      if (!popup.current?.contains(event.target as Node)) close();
    };
    const resize = () => close();
    document.addEventListener("pointerdown", outside);
    window.addEventListener("scroll", scroll, true);
    window.addEventListener("resize", resize);
    return () => {
      document.removeEventListener("pointerdown", outside);
      window.removeEventListener("scroll", scroll, true);
      window.removeEventListener("resize", resize);
    };
  }, [open]);

  function key(event: KeyboardEvent<HTMLDivElement>) {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    const item = rows[activeIndex]?.node;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
      return;
    }
    if (event.key === "Tab") {
      close(true);
      return;
    }
    if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      event.stopPropagation();
      const next =
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? rows.length - 1
            : Math.max(0, Math.min(rows.length - 1, activeIndex + (event.key === "ArrowDown" ? 1 : -1)));
      if (rows[next]) setActive(rows[next].node.path);
      query.current = { text: "", at: 0 };
      return;
    }
    if (event.key === "ArrowRight" && item?.children) {
      event.preventDefault();
      event.stopPropagation();
      if (!expanded.has(item.path)) toggle(item.path, true);
      else if (item.children[0]) setActive(item.children[0].path);
      return;
    }
    if (event.key === "ArrowLeft" && item) {
      event.preventDefault();
      event.stopPropagation();
      if (item.children && expanded.has(item.path)) toggle(item.path, false);
      else {
        const parent = item.path.split("/").slice(0, -1).join("/");
        if (rows.some(({ node }) => node.path === parent)) setActive(parent);
      }
      return;
    }
    if ((event.key === "Enter" || event.key === " ") && item) {
      event.preventDefault();
      event.stopPropagation();
      if (item.children) toggle(item.path);
      else void choose(item.path);
      return;
    }
    if (event.key.length === 1 && !event.altKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      const now = Date.now();
      const previous = now - query.current.at < 700 ? query.current.text : "";
      const character = event.key.toLocaleLowerCase();
      const text = previous && [...previous].every((value) => value === character) ? character : previous + character;
      query.current = { text, at: now };
      const next = matchSelectValue(
        rows.map(({ node }) => ({ value: node.path, label: node.name })),
        text,
        active,
      );
      if (next !== undefined) setActive(next);
    }
  }
  return (
    <>
      <nav ref={nav} className="code-path code-breadcrumbs" aria-label={t("文件路径")} title={path}>
        <ol>
          {segments.length ? (
            segments.map((segment, index) => (
              <li key={segment.path}>
                {index > 0 && (
                  <span className="code-breadcrumb-divider" aria-hidden="true">
                    /
                  </span>
                )}
                <button
                  type="button"
                  className="code-breadcrumb-button"
                  aria-current={segment.file ? "page" : undefined}
                  aria-label={t("浏览 {name} 的同级文件", { name: segment.name })}
                  aria-haspopup="tree"
                  aria-expanded={open && context.index === index}
                  aria-controls={popupId}
                  title={segment.path}
                  disabled={unavailable}
                  onClick={(event) => show(event.currentTarget, segment.parent, segment.path, index)}
                  onKeyDown={(event) => {
                    if (!event.nativeEvent.isComposing && event.key === "ArrowDown") {
                      event.preventDefault();
                      show(event.currentTarget, segment.parent, segment.path, index);
                    }
                  }}
                >
                  {segment.name}
                </button>
              </li>
            ))
          ) : (
            <li>
              <button
                type="button"
                className="code-breadcrumb-button"
                aria-haspopup="tree"
                aria-expanded={open}
                aria-controls={popupId}
                disabled={unavailable}
                onClick={(event) => show(event.currentTarget, "", "", -1)}
              >
                {t("选择文件")}
              </button>
            </li>
          )}
        </ol>
      </nav>
      {container &&
        createPortal(
          <div
            ref={popup}
            id={popupId}
            role="tree"
            aria-label={t("{path} 的文件", { path: context.parent || t("项目根目录") })}
            tabIndex={0}
            aria-activedescendant={activeIndex >= 0 ? `${id}-row-${activeIndex}` : undefined}
            popover={nativePopover ? "auto" : undefined}
            data-fallback-open={(!nativePopover && open) || undefined}
            data-breadcrumb-menu=""
            className="code-breadcrumb-menu"
            style={position}
            onToggle={(event) => setOpen(event.newState === "open")}
            onKeyDown={key}
          >
            {rows.map(({ node, depth }, index) => {
              const folder = !!node.children;
              const expandedHere = expanded.has(node.path);
              return (
                <div
                  key={node.path}
                  id={`${id}-row-${index}`}
                  role="treeitem"
                  aria-level={depth + 1}
                  aria-expanded={folder ? expandedHere : undefined}
                  aria-selected={folder ? undefined : node.path === path}
                  data-path={node.path}
                  data-row-index={index}
                  className={`code-breadcrumb-item${active === node.path ? " is-active" : ""}`}
                  style={{ paddingLeft: 8 + depth * 14 }}
                  title={node.path}
                  onPointerMove={(event) => {
                    if (event.pointerType !== "touch") setActive(node.path);
                  }}
                  onPointerDown={(event) => {
                    if (event.pointerType !== "touch") event.preventDefault();
                  }}
                  onClick={() => {
                    setActive(node.path);
                    if (folder) toggle(node.path);
                    else void choose(node.path);
                  }}
                >
                  {folder ? (
                    expandedHere ? (
                      <CaretDown size={11} aria-hidden="true" />
                    ) : (
                      <CaretRight size={11} aria-hidden="true" />
                    )
                  ) : (
                    <span className="code-tree-spacer" />
                  )}
                  {folder ? (
                    expandedHere ? (
                      <FolderOpen size={15} aria-hidden="true" />
                    ) : (
                      <Folder size={15} aria-hidden="true" />
                    )
                  ) : (
                    <FileCode size={15} aria-hidden="true" />
                  )}
                  <span>{node.name}</span>
                </div>
              );
            })}
            {!rows.length && <p className="code-breadcrumb-empty">{t("这个目录没有文件。")}</p>}
          </div>,
          container,
        )}
    </>
  );
}
