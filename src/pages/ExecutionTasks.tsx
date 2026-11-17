import { memo, useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowsClockwise, CaretRight, FileArrowUp, Play, Plus, Stop } from "@phosphor-icons/react";
import { demo, errorMessage, native } from "../bridge";
import { Editor } from "../Editor";
import { exec, execActive, initialExecRequest, parseExecRequest, type ExecJob, type ExecStatus } from "../exec";
import { useI18n } from "../i18n";
import { JsonCode } from "../JsonCode";
import { useSerialSnapshot } from "../serial";
import { Button, Dialog, Dot, IconButton, Input, PageHeader } from "../ui";
import "./ExecutionTasks.css";

const statusLabels: Record<ExecStatus, string> = {
  queued: "排队中",
  running: "执行中",
  awaiting_approval: "等待授权",
  succeeded: "执行成功",
  failed: "执行失败",
  cancelled: "已取消",
  interrupted: "执行中断",
};

// Accepted requests are immutable for a job ID, even as polling replaces its status.
const RequestJSON = memo(
  function RequestJSON({ job }: { job: ExecJob }) {
    return <JsonCode text={JSON.stringify(job.request, null, 2)} />;
  },
  (previous, next) => previous.job.id === next.job.id,
);

export function ExecutionTasksPage({ onService }: { onService: () => void }) {
  const { t, language } = useI18n();
  const { snapshot, error: serviceError } = useSerialSnapshot();
  const running = native && Boolean(snapshot?.running);
  const [text, setText] = useState(initialExecRequest);
  const [output, setOutput] = useState("");
  const [validation, setValidation] = useState("");
  const [error, setError] = useState("");
  const [creating, setCreating] = useState(false);
  const [submitError, setSubmitError] = useState("");
  const [historyError, setHistoryError] = useState("");
  const [feedback, setFeedback] = useState("");
  const [discovery, setDiscovery] = useState<string | null>(null);
  const [jobs, setJobs] = useState<ExecJob[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [busyJob, setBusyJob] = useState("");
  const [revision, setRevision] = useState(0);
  const [visible, setVisible] = useState(() => document.visibilityState !== "hidden");
  const mounted = useRef(true);
  const submittingRef = useRef(false);
  useEffect(() => {
    mounted.current = true;
    const visibility = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", visibility);
    return () => {
      mounted.current = false;
      document.removeEventListener("visibilitychange", visibility);
    };
  }, []);

  // Bounded requests only. Navigation or hiding the window retires this polling loop.
  useEffect(() => {
    if (!running || !visible) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      setLoading(true);
      try {
        const next = await exec.jobs();
        if (disposed) return;
        setJobs(next);
        setLoaded(true);
        setHistoryError("");
      } catch (cause) {
        if (!disposed) setHistoryError(errorMessage(cause));
      } finally {
        if (!disposed) {
          setLoading(false);
          timer = setTimeout(() => void poll(), 2500);
        }
      }
    };
    void poll();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [running, visible, revision]);

  const date = (value: number | null, missing: string) =>
    value === null ? t(missing) : new Date(value).toLocaleString(language === "en" ? "en-US" : "zh-CN");
  const duration = (job: ExecJob) => {
    if (job.startedAt === null) return "—";
    const seconds = Math.max(0, ((job.finishedAt ?? Date.now()) - job.startedAt) / 1000);
    return t("{seconds} 秒", { seconds: seconds.toLocaleString(language, { maximumFractionDigits: 1 }) });
  };
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (submittingRef.current || !running) return;
    let envelope;
    try {
      envelope = parseExecRequest(text);
      if (output.includes("\0")) throw new Error("输出文件路径不能包含空字符。");
      setValidation("");
    } catch (cause) {
      setValidation(errorMessage(cause));
      return;
    }
    submittingRef.current = true;
    setSubmitting(true);
    setSubmitError("");
    setFeedback("");
    setDiscovery(null);
    try {
      const response = await exec.submit(envelope, output);
      if (!mounted.current) return;
      if (!response.job && response.result === undefined) throw new Error("执行服务未返回任务或结果，请刷新历史确认请求状态。");
      setFeedback(response.job ? t("请求已提交，执行状态以服务记录为准。") : t("观察请求已完成，不会创建任务历史。"));
      // Observation results are shown only for this submission, never persisted in history.
      if (!response.job && response.result !== undefined)
        setDiscovery(JSON.stringify(response.result, null, 2));
      // Accepted actions appear in the list; observations keep the dialog open to show their result.
      if (response.job) setCreating(false);
      setRevision((value) => value + 1);
    } catch (cause) {
      if (mounted.current) setSubmitError(errorMessage(cause));
    } finally {
      submittingRef.current = false;
      if (mounted.current) setSubmitting(false);
    }
  }
  async function act(job: ExecJob, action: "cancel" | "reveal") {
    if (busyJob) return;
    setBusyJob(job.id);
    setError("");
    try {
      if (action === "cancel") {
        const { job: updated } = await exec.cancel(job.id);
        if (!mounted.current) return;
        setJobs((all) => all.map((item) => (item.id === updated.id ? updated : item)));
        setRevision((value) => value + 1);
      } else await exec.reveal(job.id);
    } catch (cause) {
      if (mounted.current) setError(errorMessage(cause));
    } finally {
      if (mounted.current) setBusyJob("");
    }
  }

  const openCreate = () => {
    setSubmitError("");
    setFeedback("");
    setDiscovery(null);
    setCreating(true);
  };

  return (
    <>
      <PageHeader crumbs={[{ label: t("执行任务") }]} />
      <div className="page list-page execution-page">
        <div className="page-description">
          <span>{t("通过 JSON 创建任务，与 gizmoth request 共用执行器。所有执行任务的请求和状态保存在本机，可随时展开查看。")}</span>
        </div>
        {demo && (
          <p className="notice" role="note">
            {t("演示模式：任务仅模拟，请求历史保存在此浏览器，不会操作设备或创建输出文件。")}
          </p>
        )}
        {native && !snapshot && !serviceError && <p role="status">{t("正在读取执行服务…")}</p>}
        {native && snapshot && !snapshot.running && (
          <div className="notice execution-service" role="status">
            <span>{t("执行服务未运行。请在服务设置中检查服务状态后重试。")}</span>
            <Button controlSize="sm" onClick={onService}>
              {t("服务设置")}
            </Button>
          </div>
        )}
        {(error || historyError || serviceError) && (
          <p className="error banner" role="alert">
            {t(error || historyError || serviceError || "")}
          </p>
        )}
        <div className="page-list-toolbar">
          <button type="button" className="text-action" disabled={!running} onClick={openCreate}>
            <Plus size={14} aria-hidden="true" />
            {t("新建任务")}
          </button>
          {!creating && feedback && (
            <span className="execution-feedback" role="status">
              {feedback}
            </span>
          )}
          <span className="page-list-summary">{loaded && t("{count} 个任务", { count: jobs.length })}</span>
          <IconButton
            aria-label={t("刷新任务历史")}
            title={t("刷新任务历史")}
            disabled={!running || loading}
            onClick={() => setRevision((value) => value + 1)}
          >
            <ArrowsClockwise size={15} className={loading ? "spin" : undefined} />
          </IconButton>
        </div>
        <section className="execution-section" aria-label={t("任务历史")}>
          {!loaded ? (
            <p className="execution-empty" role="status">
              {t(running ? "正在读取任务历史…" : "服务可用时将读取已保存的任务历史。")}
            </p>
          ) : !jobs.length ? (
            <div className="execution-empty">
              <p>{t("还没有执行任务")}</p>
              <p className="execution-hint">
                {t("提交执行请求后会显示在这里。状态查询和日志读取等观察操作不会创建历史。")}
              </p>
            </div>
          ) : (
            <div className="card execution-history">
              {jobs.map((job) => (
                <details key={job.id} className="execution-job" data-job-id={job.id}>
                  <summary>
                    <CaretRight size={14} className="execution-caret" aria-hidden="true" />
                    <span className="execution-job-name" title={job.name || job.operation}>
                      {job.name || job.operation}
                    </span>
                    <code className="execution-job-operation">{job.operation}</code>
                    <span className="execution-state" data-status={job.status}>
                      <Dot
                        tone={
                          job.status === "succeeded"
                            ? "ok"
                            : ["failed", "interrupted", "awaiting_approval"].includes(job.status)
                              ? "warn"
                              : execActive(job)
                                ? "idle"
                                : "off"
                        }
                      />
                      {t(statusLabels[job.status] ?? job.status)}
                    </span>
                    <time dateTime={new Date(job.createdAt).toISOString()}>{date(job.createdAt, "")}</time>
                  </summary>
                  <div className="execution-job-detail">
                    <dl className="execution-facts">
                      <div>
                        <dt>{t("任务 ID")}</dt>
                        <dd>
                          <code>{job.id}</code>
                        </dd>
                      </div>
                      <div>
                        <dt>{t("创建时间")}</dt>
                        <dd>{date(job.createdAt, "")}</dd>
                      </div>
                      <div>
                        <dt>{t("开始时间")}</dt>
                        <dd>{date(job.startedAt, "尚未开始")}</dd>
                      </div>
                      <div>
                        <dt>{t("结束时间")}</dt>
                        <dd>{date(job.finishedAt, "尚未结束")}</dd>
                      </div>
                      <div>
                        <dt>{t("耗时")}</dt>
                        <dd>{duration(job)}</dd>
                      </div>
                      {job.sourceJobId && (
                        <div>
                          <dt>{t("关联任务 ID")}</dt>
                          <dd>
                            <code>{job.sourceJobId}</code>
                          </dd>
                        </div>
                      )}
                    </dl>
                    {job.error && (
                      <div className="execution-job-error">
                        <span>{t("执行错误")}</span>
                        <p>{job.error}</p>
                      </div>
                    )}
                    {job.output && (
                      <div className="execution-output-entry">
                        <div>
                          <span>{t("输出文件")}</span>
                          <code>{job.output}</code>
                        </div>
                        <Button
                          controlSize="sm"
                          disabled={demo || !running || execActive(job) || Boolean(busyJob)}
                          title={demo ? t("演示模式不会创建输出文件。") : undefined}
                          onClick={() => void act(job, "reveal")}
                        >
                          <FileArrowUp size={14} aria-hidden="true" />
                          {t("在文件夹中显示")}
                        </Button>
                      </div>
                    )}
                    <div className="execution-json-heading">
                      <h3>{t("已接受的请求 JSON")}</h3>
                      {execActive(job) && (
                        <Button
                          controlSize="sm"
                          disabled={!running || Boolean(busyJob)}
                          onClick={() => void act(job, "cancel")}
                        >
                          <Stop size={13} aria-hidden="true" />
                          {t(busyJob === job.id ? "正在取消…" : "取消任务")}
                        </Button>
                      )}
                    </div>
                    <pre className="execution-request" tabIndex={0} aria-label={t("已接受的请求 JSON")}>
                      <RequestJSON job={job} />
                    </pre>
                  </div>
                </details>
              ))}
            </div>
          )}
        </section>
      </div>
      {creating && (
        <Dialog title={t("新建任务")} wide className="execution-dialog" busy={submitting} onClose={() => setCreating(false)}>
          <form className="execution-form" onSubmit={(event) => void submit(event)}>
            <div className="execution-field">
              <span id="execution-json-label">{t("请求 JSON")}</span>
              <p id="execution-json-help" className="execution-hint">
                {t("先执行 describe 查看支持的操作，再填写 spec。任务名称可在 metadata.name 中设置。")}
              </p>
              <div
                className={`execution-editor${validation ? " is-invalid" : ""}`}
                role="group"
                aria-labelledby="execution-json-label"
                aria-describedby={`execution-json-help${validation ? " execution-validation" : ""}`}
                aria-invalid={Boolean(validation)}
              >
                <Editor
                  path="request.json"
                  language="json"
                  content={text}
                  disabled={submitting}
                  onChange={(value) => {
                    setText(value);
                    setValidation("");
                  }}
                />
              </div>
              {validation && (
                <p id="execution-validation" className="error" role="alert">
                  {t(validation)}
                </p>
              )}
            </div>
            <div className="execution-field">
              <label htmlFor="execution-output">{t("输出文件路径（可选）")}</label>
              <Input
                id="execution-output"
                value={output}
                disabled={submitting}
                onChange={(event) => setOutput(event.target.value)}
                placeholder={t("例如 /Users/me/results/task.json")}
                aria-describedby="execution-output-help"
              />
              <p id="execution-output-help" className="execution-hint">
                {t("对应 gizmoth request --output；请填写尚不存在的新文件路径。任务历史将显示该文件的入口。")}
              </p>
            </div>
            {submitError && (
              <p className="error" role="alert">
                {t(submitError)}
              </p>
            )}
            {feedback && (
              <p className="execution-feedback" role="status">
                {feedback}
              </p>
            )}
            {discovery && (
              <details className="execution-discovery" open>
                <summary>{t("本次查询结果")}</summary>
                <pre tabIndex={0} aria-label={t("本次查询结果")}>
                  <JsonCode text={discovery} />
                </pre>
              </details>
            )}
            <p className="execution-hint">{t("历史仅保存请求、状态、时间、错误和输出路径，不保存标准输出或日志。")}</p>
            <div className="dialog-actions">
              <Button type="button" disabled={submitting} onClick={() => setCreating(false)}>
                {t("取消")}
              </Button>
              <Button type="submit" variant="primary" disabled={!running || submitting}>
                <Play size={14} aria-hidden="true" />
                {t(submitting ? "正在提交…" : "执行 JSON")}
              </Button>
            </div>
          </form>
        </Dialog>
      )}
    </>
  );
}
