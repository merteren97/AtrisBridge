import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import AtrisAccount from "./AtrisAccount";
import DesktopActivityCenter from "./DesktopActivityCenter";
import UpdateCenter from "./UpdateCenter";
import "./cloud.css";
import "./styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
    <AtrisAccount />
    <DesktopActivityCenter />
    <UpdateCenter />
  </StrictMode>,
);
