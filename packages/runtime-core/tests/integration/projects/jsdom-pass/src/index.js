import { JSDOM } from "jsdom";

const dom = new JSDOM("<main><p id=\"agent\">jsdom</p></main>");
dom.window.document.querySelector("#agent").setAttribute("data-runtime", "agentos");
console.log(JSON.stringify({
  text: dom.window.document.querySelector("#agent").textContent,
  runtime: dom.window.document.querySelector("#agent").getAttribute("data-runtime"),
}));
