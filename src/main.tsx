import React from "react";
import ReactDOM from "react-dom/client";
import "./styles.css";
import { IS_TAURI, TOKEN, WINDOW } from "./lib/transport";
import Panel from "./windows/Panel";
import Settings from "./windows/Settings";

if (WINDOW === "panel") {
  document.documentElement.classList.add("panel-root");
  if (IS_TAURI) document.documentElement.classList.add("tauri-transparent"); // 真透明窗口，不需要整块底色
} else document.title = "SayDo · 设置";

if (import.meta.env.DEV && !TOKEN) import("./dev/mock").then((m) => m.installPanelPreview());

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{WINDOW === "settings" ? <Settings /> : <Panel />}</React.StrictMode>,
);
