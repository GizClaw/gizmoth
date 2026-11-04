import React from "react";
import { createRoot } from "react-dom/client";
import { installProviderFetch } from "./http";
import { App } from "./App";
import "./styles.css";

installProviderFetch();

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
