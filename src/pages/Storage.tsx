import { useCallback, useState } from "react";
import {
  CircleNotch,
  Database,
  Disc,
  DownloadSimple,
  FolderOpen,
  HardDrive,
  HardDrives,
  Package,
  Toolbox,
  Warning,
  type Icon,
} from "@phosphor-icons/react";
import { demo, errorMessage, native } from "../bridge";
import {
  compile,
  formatSize,
  useCompileSnapshot,
  type CompileSnapshot,
  type CompileStorage,
  type StorageItem,
} from "../compile";
import { useI18n } from "../i18n";
import { Button, Dialog, IconButton, PageHeader } from "../ui";
import "./Settings.css";
import "./Storage.css";

const kinds: Record<StorageItem["id"], { label: string; Icon: Icon }> = {
  image: { label: "系统镜像", Icon: Disc },
  slots: { label: "Slot 工作盘", Icon: HardDrive },
  caches: { label: "共享缓存", Icon: Database },
  artifacts: { label: "产物", Icon: Package },
  packages: { label: "开发包", Icon: Toolbox },
  assets: { label: "下载文件", Icon: DownloadSimple },
};

function total(storage: CompileStorage) {
  return storage.items.reduce((sum, item) => sum + item.bytes, 0);
}

/** The name below the working directory, as shown in the tree; folders end in `/`. */
function folder(item: StorageItem) {
  const name = item.path.split(/[\\/]/).filter(Boolean).at(-1) ?? item.id;
  return item.id === "packages" ? name : `${name}/`;
}

/** Settings › Storage: GizMoth's working directory for compile data. */
export function StoragePage() {
  const { t } = useI18n();
  const { snapshot, error: loadError } = useCompileSnapshot();
  const [error, setError] = useState("");
  const [target, setTarget] = useState<{ path: string | null; label: string } | null>(null);
  const [choosing, setChoosing] = useState(false);
  const storage = snapshot?.storage;
  const moving = storage?.moving ?? null;
  const run = useCallback(async (operation: () => Promise<unknown>) => {
    setError("");
    try {
      await operation();
    } catch (e) {
      setError(errorMessage(e));
    }
  }, []);
  const choose = () => {
    if (!storage) return;
    setChoosing(true);
    void run(async () => {
      const path = await compile.chooseDirectory(storage.root);
      if (path) setTarget({ path, label: path });
    }).finally(() => setChoosing(false));
  };
  return (
    <>
      <PageHeader crumbs={[{ label: t("设置") }, { label: t("存储") }]} />
      <div className="page settings-page storage-page">
        <div className="page-description">
          <p>{t("选择 GizMoth 存放数据的工作目录。")}</p>
          <span>
            {t(
              "编译用的系统镜像、build slot 工作盘、共享缓存和产物都放在这里。可以选择外接硬盘或空间更大的磁盘，更改时现有数据会一起移过去。",
            )}
          </span>
        </div>
        {demo && <p className="notice">{t("演示模式：移动只是模拟，不会读写本机文件。")}</p>}
        {native && !snapshot && !loadError && (
          <p className="storage-muted" role="status">
            {t("正在读取工作目录…")}
          </p>
        )}
        {loadError && !snapshot && (
          <p className="error banner" role="alert">
            {t("无法读取工作目录：{error}", { error: loadError })}
          </p>
        )}
        {error && (
          <p className="error banner" role="alert">
            {t(error)}
          </p>
        )}
        {storage && !storage.available && !moving && (
          <p className="notice storage-notice" role="alert">
            <Warning size={15} aria-hidden="true" />
            {t("找不到工作目录 {path}。请连接它所在的磁盘，或选择其他位置；新位置会从空目录开始。", {
              path: storage.root,
            })}
          </p>
        )}
        {storage?.error && !moving && (
          <p className="error banner" role="alert">
            {storage.error === "cancelled"
              ? t("已取消移动，数据仍在原来的位置。")
              : t("移动失败，数据仍在原来的位置：{error}", { error: t(storage.error) })}
          </p>
        )}
        {snapshot && storage && (
          <section className="settings-group">
            <StorageTree
              snapshot={snapshot}
              storage={storage}
              moving={Boolean(moving)}
              choosing={choosing}
              onChoose={choose}
              onRestore={() => setTarget({ path: null, label: storage.default })}
              onReveal={(path) => void run(() => compile.reveal(path))}
            />
            {moving && <MoveProgress moving={moving} onCancel={() => void run(compile.cancelStorageMove)} />}
            <p className="storage-hint">
              {t(
                "工作盘和系统镜像是稀疏文件，只按实际写入的数据占用空间。项目和设置仍保存在应用数据目录中。",
              )}
            </p>
          </section>
        )}
      </div>
      {target && storage && (
        <MoveDialog
          storage={storage}
          target={target}
          busy={snapshot ? snapshot.queue.length + snapshot.slots.filter((slot) => slot.job).length : 0}
          onClose={() => setTarget(null)}
        />
      )}
    </>
  );
}

/** The working directory and the folder of each kind of data, one row each. */
function StorageTree({
  snapshot,
  storage,
  moving,
  choosing,
  onChoose,
  onRestore,
  onReveal,
}: {
  snapshot: CompileSnapshot;
  storage: CompileStorage;
  moving: boolean;
  choosing: boolean;
  onChoose: () => void;
  onRestore: () => void;
  onReveal: (path: string) => void;
}) {
  const { t } = useI18n();
  const detail = (id: StorageItem["id"]) => {
    switch (id) {
      case "image":
        return snapshot.image.status === "ready"
          ? (snapshot.image.info?.base ?? snapshot.image.base)
          : snapshot.image.status === "building"
            ? t("正在构建")
            : t("尚未构建");
      case "slots":
        return t("{count} 个 slot", { count: snapshot.slots.length });
      case "caches":
        return t("{count} 个缓存", { count: snapshot.caches.length });
      case "artifacts":
        return t("{count} 个产物", { count: snapshot.artifacts.count });
      case "packages":
        return t("{count} 个开发包，只读磁盘", {
          count: snapshot.packages.items.filter((p) => p.state === "installed").length,
        });
      case "assets":
        return t("镜像和开发包的原始下载");
    }
  };
  return (
    <ul className="card storage-tree" aria-label={t("工作目录")}>
      <li className="storage-tree-row storage-tree-root">
        <span className="storage-tree-name">
          <HardDrives size={16} aria-hidden="true" />
          <span className="storage-tree-label">{t("工作目录")}</span>
          <span className="storage-tag">{storage.isDefault ? t("默认") : t("自定义")}</span>
        </span>
        <span className="storage-tree-path">
          <code className="storage-path" title={storage.root}>
            <bdi dir="ltr">{storage.root}</bdi>
          </code>
          {storage.free !== null && (
            <span className="storage-free">{t("可用 {size}", { size: formatSize(storage.free) })}</span>
          )}
        </span>
        <span className="storage-tree-size">{formatSize(total(storage))}</span>
        <span className="storage-tree-change">
          <Button controlSize="sm" disabled={moving || choosing} onClick={onChoose}>
            {t("更改…")}
          </Button>
        </span>
        <span className="storage-tree-actions">
          <IconButton
            aria-label={t("在访达中显示 {name}", { name: t("工作目录") })}
            title={t("在访达中显示")}
            disabled={!storage.available}
            onClick={() => onReveal(storage.root)}
          >
            <FolderOpen size={15} />
          </IconButton>
        </span>
      </li>
      {storage.items.map((item, index) => {
        const { label, Icon } = kinds[item.id];
        return (
          <li
            key={item.id}
            className={`storage-tree-row storage-tree-item${index === storage.items.length - 1 ? " last" : ""}`}
            data-storage-item={item.id}
          >
            <span className="storage-tree-name">
              <Icon size={15} aria-hidden="true" />
              <span className="storage-tree-label">{t(label)}</span>
            </span>
            <code className="storage-tree-folder" title={item.path}>
              {folder(item)}
            </code>
            <span className="storage-tree-detail">{detail(item.id)}</span>
            <span className="storage-tree-size">{formatSize(item.bytes)}</span>
            <span className="storage-tree-actions">
              <IconButton
                aria-label={t("在访达中显示 {name}", { name: t(label) })}
                title={item.exists ? t("在访达中显示") : t("还没有内容")}
                disabled={!item.exists}
                onClick={() => onReveal(item.path)}
              >
                <FolderOpen size={15} />
              </IconButton>
            </span>
          </li>
        );
      })}
      {!storage.isDefault && (
        <li className="storage-tree-row storage-tree-default">
          <span className="storage-tree-name storage-muted">{t("默认位置")}</span>
          <span className="storage-tree-path">
            <code className="storage-path storage-muted" title={storage.default}>
              <bdi dir="ltr">{storage.default}</bdi>
            </code>
          </span>
          <span className="storage-tree-restore">
            <Button controlSize="sm" variant="ghost" disabled={moving} onClick={onRestore}>
              {t("恢复默认位置")}
            </Button>
          </span>
        </li>
      )}
    </ul>
  );
}

function MoveProgress({ moving, onCancel }: { moving: NonNullable<CompileStorage["moving"]>; onCancel: () => void }) {
  const { t } = useI18n();
  const copying = moving.phase === "moving" && moving.total > 1;
  const percent = copying ? Math.min(100, Math.round((moving.done / moving.total) * 100)) : 0;
  return (
    <section className="card storage-move" aria-live="polite">
      <div className="storage-move-head">
        <CircleNotch size={14} className="spin" aria-hidden="true" />
        <span className="storage-move-title">{t("正在移动到 {path}", { path: moving.to })}</span>
        <Button controlSize="sm" variant="ghost" onClick={onCancel}>
          {t("取消")}
        </Button>
      </div>
      <div
        className="storage-progress"
        role="progressbar"
        aria-label={t("移动进度")}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={copying ? percent : undefined}
      >
        <span
          className={copying ? undefined : "indeterminate"}
          style={copying ? { width: `${percent}%` } : undefined}
        />
      </div>
      <span className="storage-muted">
        {moving.phase === "stopping"
          ? t("正在关闭空闲的虚拟机…")
          : copying
            ? t("已复制 {done} / {total}", { done: formatSize(moving.done), total: formatSize(moving.total) })
            : t("正在移动…")}
        {" · "}
        {t("新的编译会排队等待移动完成。")}
      </span>
    </section>
  );
}

function MoveDialog({
  storage,
  target,
  busy,
  onClose,
}: {
  storage: CompileStorage;
  target: { path: string | null; label: string };
  busy: number;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const restoring = target.path === null;
  const title = restoring ? t("恢复默认位置") : t("更改工作目录");
  async function move() {
    setSaving(true);
    setError("");
    try {
      await compile.moveStorage(target.path);
      onClose();
    } catch (e) {
      setError(errorMessage(e));
      setSaving(false);
    }
  }
  return (
    <Dialog title={title} onClose={onClose} busy={saving}>
      <p className="dialog-text">
        {storage.available
          ? t("系统镜像、slot 工作盘、缓存和产物（共 {size}）会移动到：", { size: formatSize(total(storage)) })
          : t("原来的工作目录不可用，GizMoth 会在这里重新开始：")}
      </p>
      <code className="storage-path storage-dialog-path">{target.label}</code>
      <p className="dialog-text storage-muted">
        {t(
          "同一磁盘内会直接移动；换到其他磁盘需要复制，可能需要几分钟。空闲的虚拟机会先关闭，移动期间新的编译会排队等待。",
        )}
      </p>
      {busy > 0 && (
        <p className="notice" role="status">
          {t("还有 {count} 个编译在进行或排队，等它们结束后才能移动。", { count: busy })}
        </p>
      )}
      {error && (
        <p className="error banner" role="alert">
          {t(error)}
        </p>
      )}
      <div className="dialog-actions">
        <Button disabled={saving} onClick={onClose}>
          {t("取消")}
        </Button>
        <Button variant="primary" disabled={saving || busy > 0} onClick={() => void move()}>
          {restoring ? t("恢复并移动") : t("移动")}
        </Button>
      </div>
    </Dialog>
  );
}
