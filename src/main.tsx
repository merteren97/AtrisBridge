import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import AtrisAccount from "./AtrisAccount";
import UpdateCenter from "./UpdateCenter";
import "./styles.css";
import "./cloud.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
    <AtrisAccount />
    <UpdateCenter />
  </StrictMode>,
);
