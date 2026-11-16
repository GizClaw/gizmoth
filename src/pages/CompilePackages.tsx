import { useId, useState } from "react";
import { ArrowClockwise, CaretRight, CircleNotch, DownloadSimple, Trash } from "@phosphor-icons/react";
import { errorMessage } from "../bridge";
import { compile, formatSize, type CompilePackage, type CompilePackageKey, type CompileSnapshot } from "../compile";
import { useI18n } from "../i18n";
import { Button, Dialog, relativeTime } from "../ui";
import { WorkspaceActions } from "./WorkspaceActions";

type Run = (operation: () => Promise<unknown>) => Promise<boolean>;
const labels: Record<CompilePackage["state"], string> = {
  pending: "等待下载",
  downloading: "下载中",
  downloaded: "已下载",
  packing: "正在准备",
  installed: "已安装",
  failed: "失败",
  unsupported: "不支持",
  removed: "已删除",
};

function itemKey(item: CompilePackage, platform: string) {
  return `${item.platform ?? platform}/${item.kind}/${item.id}/${item.version}`;
}
function identity(item: CompilePackage, platform: string): CompilePackageKey {
  return { kind: item.kind, id: item.id, version: item.version, platform: item.platform ?? platform };
}

/** Packages selected for the current Linux build environment. */
export function PackagesSection({ snapshot, run }: { snapshot: CompileSnapshot; run: Run }) {
  const { t } = useI18n();
  const prefix = useId();
  const { packages, storage } = snapshot;
  const { registry, pack } = packages;
  const [working, setWorking] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const toggleDetails = (key: string) => {
    setExpanded((previous) => {
      const next = new Set(previous);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };
  const [removing, setRemoving] = useState<CompilePackage | null>(null);
  const [actionError, setActionError] = useState<{ key: string; message: string } | null>(null);
  const reasonText = (reason: string) => {
    const messages: Record<string, string> = {
      "This package is already removed; reinstall to restore it": "这个包已删除，可重新安装以恢复。",
      "Required Linux runtime/bootstrap input; keep it installed": "Linux 运行环境的基础依赖不能单独删除。",
      "Only the default Linux root is assembled; reinstall its default version":
        "只使用默认的 Linux 系统层，请重新安装默认版本。",
      "Wait for the working directory move to finish": "请等待工作目录移动完成。",
      "Wait for downloads and runtime preparation to finish": "请等待下载和运行环境准备完成。",
      "Wait for builds and VM shutdowns to finish": "请等待编译任务和虚拟机关闭完成。",
      "Refresh the registry before managing packages": "请先检查更新，读取配方目录。",
      "The requested platform/version is not a managed Linux recipe": "该配方不适用于当前编译环境",
    };
    const dependency = /^Required by (.+); remove that package first$/.exec(reason);
    if (dependency) return t("仍被 {name} 使用，请先删除该包。", { name: dependency[1] });
    const missing = /^Reinstall required toolchain (.+) first$/.exec(reason);
    if (missing) return t("请先重新安装依赖 {name}。", { name: missing[1] });
    return t(messages[reason] ?? reason);
  };
  const items = packages.items.filter((item) => (item.platform ?? packages.platform) === packages.platform);
  const paused = packages.paused ?? !packages.active;
  const locked = working !== null || Boolean(storage.moving) || !storage.available;
  const removingItem =
    removing &&
    (packages.items.find((item) => itemKey(item, packages.platform) === itemKey(removing, packages.platform)) ??
      removing);
  const mutate = async (operation: () => Promise<unknown>, key: string) => {
    setWorking(key);
    try {
      return await run(operation);
    } finally {
      setWorking(null);
    }
  };
  const mutateItem = async (item: CompilePackage, operation: () => Promise<unknown>) => {
    setWorking(itemKey(item, packages.platform));
    setActionError(null);
    try {
      await operation();
      return true;
    } catch (error) {
      setActionError({ key: itemKey(item, packages.platform), message: errorMessage(error) });
      return false;
    } finally {
      setWorking(null);
    }
  };
  const packText =
    pack.state === "ready"
      ? t("运行环境已就绪")
      : pack.state === "packing"
        ? t("正在准备运行环境…")
        : pack.state === "stale"
          ? t("下载完成后自动准备运行环境")
          : pack.state === "failed"
            ? t("运行环境准备失败：{error}", { error: pack.error })
            : t("下载后将准备 Linux 运行环境");

  return (
    <section className="settings-group compile-group">
      <h2>{t("下载管理")}</h2>
      <div className="card compile-packages-card">
        <div className="compile-packages-head">
          <div className="compile-packages-summary">
            <span>
              {t("自动管理当前 Linux 编译环境的依赖，删除后不再自动下载。工具只读挂载，构建与缓存写入宿主工作目录。")}
            </span>
            <span className="compile-muted">
              {t(paused ? "自动下载已暂停" : packages.active ? "自动下载与更新已开启" : "尚未开始下载")}
              {" · "}
              {registry.refreshing
                ? t("正在检查更新…")
                : registry.fetchedAt
                  ? t("目录 {commit} · 检查于 {time}", {
                      commit: registry.commit?.slice(0, 7) ?? "",
                      time: relativeTime(registry.fetchedAt),
                    })
                  : t("还没有读取目录")}
            </span>
            <span className={pack.state === "failed" ? "error" : "compile-muted"} role="status">
              {packText}
            </span>
            {registry.error && (
              <span className="error" role="alert">
                {t("检查更新失败：{error}", { error: registry.error })}
              </span>
            )}
          </div>
          <div className="compile-download-actions">
            <Button
              controlSize="sm"
              disabled={locked || (!snapshot.available.ok && paused)}
              onClick={() => void mutate(paused ? compile.prepare : compile.pausePackages, "@pause")}
            >
              {t(paused ? "恢复下载" : "暂停下载")}
            </Button>
            <Button
              controlSize="sm"
              disabled={locked || registry.refreshing || paused}
              onClick={() => void mutate(compile.refreshPackages, "@refresh")}
            >
              {t("检查更新")}
            </Button>
          </div>
        </div>
        {items.length ? (
          <ul className="compile-package-list" aria-label={t("下载管理")}>
            {items.map((item) => {
              const key = itemKey(item, packages.platform);
              const processing = working === key;
              const busy =
                processing || ((item.repairing || !paused) && ["downloading", "packing"].includes(item.state));
              const percent =
                item.state === "downloading" && item.total
                  ? Math.min(100, Math.round(((item.done ?? 0) / item.total) * 100))
                  : null;
              const locations = item.linuxPaths?.length
                ? item.linuxPaths
                : [item.installPath ?? item.path].filter(Boolean);
              const open = expanded.has(key);
              const detailsId = `${prefix}-${encodeURIComponent(key)}-details`;
              const preview = !item.path && item.state === "unsupported";
              const error = actionError?.key === key ? actionError.message : preview ? null : item.error;
              return (
                <li key={key} className="compile-package-row">
                  <button
                    type="button"
                    className="compile-package-disclosure"
                    aria-expanded={open}
                    aria-controls={detailsId}
                    onClick={() => toggleDetails(key)}
                  >
                    <CaretRight size={14} className="compile-package-chevron" aria-hidden="true" />
                    <span className="compile-package-name">
                      <span className="compile-package-title" title={item.name || item.id}>
                        {item.name || item.id}
                      </span>
                      <code title={t("配方版本")}>{item.version}</code>
                      {item.default && <span className="compile-tag">{t("默认")}</span>}
                    </span>
                    <span className="compile-package-detail-label">{t(open ? "收起" : "详情")}</span>
                  </button>
                  <span className="compile-package-size">
                    {item.state === "downloading"
                      ? `${formatSize(item.done ?? 0)} / ${formatSize(item.total ?? item.download)}`
                      : formatSize(item.download)}
                  </span>
                  <span className={`compile-package-state ${item.state}`}>
                    {busy && <CircleNotch size={12} className="spin" aria-hidden="true" />}
                    {t(
                      paused && !item.repairing && ["pending", "downloading"].includes(item.state)
                        ? "已暂停"
                        : labels[item.state],
                    )}
                    {percent !== null && ` ${percent}%`}
                  </span>
                  <div
                    className="compile-package-actions"
                    title={item.reinstallReason ? reasonText(item.reinstallReason) : undefined}
                  >
                    <WorkspaceActions
                      label={t("管理 {name} {version}", { name: item.name || item.id, version: item.version })}
                      disabled={locked || packages.busy || !snapshot.available.ok || item.state === "unsupported"}
                      actions={[
                        {
                          label: item.state === "removed" ? "安装" : "重新安装",
                          icon: item.state === "removed" ? <DownloadSimple size={14} /> : <ArrowClockwise size={14} />,
                          disabled: item.canReinstall !== true,
                          title: item.reinstallReason ? reasonText(item.reinstallReason) : undefined,
                          onSelect: () =>
                            void mutateItem(item, () => compile.reinstallPackage(identity(item, packages.platform))),
                        },
                        {
                          label: "删除",
                          icon: <Trash size={14} />,
                          disabled: item.canRemove !== true,
                          title: item.removeReason ? reasonText(item.removeReason) : undefined,
                          danger: true,
                          separator: true,
                          onSelect: () => {
                            setActionError(null);
                            setRemoving(item);
                          },
                        },
                      ]}
                    />
                  </div>
                  <div id={detailsId} className="compile-package-details" hidden={!open}>
                    <dl className="compile-package-locations">
                      <div>
                        <dt>{t("宿主机路径")}</dt>
                        <dd>
                          {item.hostPaths?.downloads.length ? (
                            <ul className="compile-package-path" aria-label={t("下载位置")}>
                              {item.hostPaths.downloads.map((location) => (
                                <li key={location}>
                                  <code>{location}</code>
                                </li>
                              ))}
                            </ul>
                          ) : (
                            t("暂无本机文件")
                          )}
                        </dd>
                      </div>
                      {item.hostPaths?.runtime && (
                        <div>
                          <dt>{t("运行环境镜像")}</dt>
                          <dd>
                            <code>{item.hostPaths.runtime}</code>
                          </dd>
                        </div>
                      )}
                      <div>
                        <dt>{t("Linux 路径（只读）")}</dt>
                        <dd>
                          {locations.length ? (
                            <ul className="compile-package-path">
                              {locations.map((location) => (
                                <li key={location}>
                                  <code>{location}</code>
                                </li>
                              ))}
                            </ul>
                          ) : (
                            t("此平台没有 Linux 安装路径。")
                          )}
                        </dd>
                      </div>
                      {item.commandPath && (
                        <div>
                          <dt>{t("命令入口目录（PATH）")}</dt>
                          <dd>
                            <code>{item.commandPath}</code>
                          </dd>
                        </div>
                      )}
                    </dl>
                  </div>
                  {error && (!removingItem || itemKey(removingItem, packages.platform) !== key) && (
                    <p className="error compile-package-error" role="alert">
                      {reasonText(error)}
                    </p>
                  )}
                </li>
              );
            })}
          </ul>
        ) : (
          <p className="compile-download-empty">{t("此平台还没有可用配方。检查更新后会自动获取目录。")}</p>
        )}
      </div>
      {removingItem && (
        <Dialog
          title={t("删除 {name}？", { name: removingItem.name || removingItem.id })}
          onClose={() => setRemoving(null)}
          busy={working !== null}
        >
          <div className="dialog-body compile-package-confirm">
            <p>
              <strong>{removingItem.name || removingItem.id}</strong> <code>{removingItem.version}</code>
            </p>
            <p>
              {t("将移除这个包并重建只读运行环境。共享下载文件会保留，已删除的包不会再次自动下载；需要时可重新安装。")}
            </p>
            {removingItem.canRemove !== true && removingItem.removeReason && (
              <p className="compile-muted">{reasonText(removingItem.removeReason)}</p>
            )}
            {actionError?.key === itemKey(removingItem, packages.platform) && (
              <p className="error" role="alert">
                {reasonText(actionError.message)}
              </p>
            )}
          </div>
          <div className="dialog-actions">
            <Button disabled={working !== null} onClick={() => setRemoving(null)}>
              {t("取消")}
            </Button>
            <Button
              variant="danger"
              disabled={working !== null || removingItem.canRemove !== true}
              onClick={() => {
                void mutateItem(removingItem, () =>
                  compile.removePackage(identity(removingItem, packages.platform)),
                ).then((ok) => {
                  if (ok) setRemoving(null);
                });
              }}
            >
              {working !== null && <CircleNotch size={14} className="spin" aria-hidden="true" />}
              {t(working !== null ? "正在删除…" : "删除")}
            </Button>
          </div>
        </Dialog>
      )}
    </section>
  );
}
