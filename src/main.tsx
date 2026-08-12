import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import AtrisAccount from "./AtrisAccount";
import DesktopActivityCenter from "./DesktopActivityCenter";
import ProductApp from "./product/ProductApp";
import UpdateCenter from "./UpdateCenter";
import "./cloud.css";
import "./product/base.css";
import "./product/overview.css";
import "./product/workspace-settings.css";
import "./product/legacy-responsive.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ProductApp />
    <AtrisAccount />
    <DesktopActivityCenter />
    <UpdateCenter />
  </StrictMode>,
);
