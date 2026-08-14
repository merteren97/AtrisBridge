import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import DesktopActivityCenter from "./DesktopActivityCenter";
import ProductApp from "./product/ProductApp";
import { UpdateProvider } from "./UpdateCenter";
import "./cloud.css";
import "./product/base.css";
import "./product/overview.css";
import "./product/workspace-settings.css";
import "./product/legacy-responsive.css";
import "./product/runtime-polish.css";
import "./product/readability-overrides.css";
import "./product/layout-stability.css";

/* WebView2's native Ctrl/Cmd+F surface floats over the desktop product and can
 * cover Settings controls. AtrisBridge does not expose browser find semantics,
 * so keep that browser chrome out of the packaged desktop experience. */
if ("__TAURI_INTERNALS__" in window) {
  window.addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "f") {
      event.preventDefault();
    }
  });
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <UpdateProvider>
      <ProductApp />
      <DesktopActivityCenter />
    </UpdateProvider>
  </StrictMode>,
);
