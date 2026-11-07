import React from "react";
import { createRoot } from "react-dom/client";
import { installProviderFetch } from "./http";
import { App } from "./App";
import { UpdatesProvider } from "./updates";
import { initializeAppearance } from "./appearance";
import { t } from "./i18n";
const Simulator = React.lazy(() => import("./pages/Simulator").then(({ Simulator }) => ({ default: Simulator })));
import "./typography.css";
import "./styles.css";

if (!new URLSearchParams(location.search).has("simulator")) installProviderFetch();

void initializeAppearance().then(() =>
  createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      {new URLSearchParams(location.search).has("simulator") ? (
        <React.Suspense fallback={<div className="editor-loading">{t("正在打开模拟器…")}</div>}>
          <Simulator />
        </React.Suspense>
      ) : (
        <UpdatesProvider><App /></UpdatesProvider>
      )}
    </React.StrictMode>,
  ),
);
