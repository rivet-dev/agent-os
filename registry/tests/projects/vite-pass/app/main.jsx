import React from "react";
import { createRoot } from "react-dom/client";

function App() {
	return React.createElement("div", null, "Hello from Vite");
}

createRoot(document.getElementById("root")).render(React.createElement(App));
