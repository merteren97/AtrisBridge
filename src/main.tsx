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

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <UpdateProvider>
      <ProductApp />
      <DesktopActivityCenter />
    </UpdateProvider>
  </StrictMode>,
);
