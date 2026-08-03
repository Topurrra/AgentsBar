import { mount } from "svelte";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./app.css";
import App from "./App.svelte";
import Widget from "./lib/Widget.svelte";

// One frontend, two windows. The popover ("main") and the desktop widget ("widget")
// share the bundle; the window label picks the view, so there is exactly one copy of the
// data plumbing and the design tokens.
const target = document.getElementById("app");
const currentWindow = getCurrentWindow();

// The widget is a transparent native window, while the popover is not. Mark the document
// before mounting so the shared global CSS can leave the widget's corner pixels clear.
document.documentElement.dataset.window = currentWindow.label;

export default currentWindow.label === "widget"
  ? mount(Widget, { target })
  : mount(App, { target });
