import { createElement } from "react";
import { createRoot } from "react-dom/client";

import App from "./App.js";

import "./styles.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("Could not find the container element.");
}

const root = createRoot(container);

root.render(createElement(App));
