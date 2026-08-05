import * as cheerio from "cheerio";
import { DOMParser } from "linkedom";
import { marked } from "marked";
import MarkdownIt from "markdown-it";
import { parse as parseCsv } from "csv-parse/sync";
import { XMLParser } from "fast-xml-parser";
import protobuf from "protobufjs";
import Ajv from "ajv";

const $ = cheerio.load("<main><p>agent</p></main>");
$("p").attr("data-ok", "yes");
const linkedom = new DOMParser().parseFromString("<p>linkedom</p>", "text/html");
const csv = parseCsv("name,count\nworker,3", { columns: true });
const xml = new XMLParser().parse("<root><agent enabled=\"true\">one</agent></root>");
const root = protobuf.parse('syntax = "proto3"; message Agent { string name = 1; uint32 count = 2; }').root;
const Agent = root.lookupType("Agent");
const decoded = Agent.decode(Agent.encode({ name: "worker", count: 3 }).finish());
const validate = new Ajv().compile({ type: "object", required: ["tool"], properties: { tool: { type: "string" } } });

console.log(JSON.stringify({
  cheerio: $("p").attr("data-ok"),
  linkedom: linkedom.querySelector("p").textContent,
  marked: marked("**agent**").trim(),
  markdownIt: new MarkdownIt().renderInline("_tool_"),
  csv: csv[0],
  xml: xml.root.agent,
  protobuf: decoded,
  ajv: [validate({ tool: "search" }), validate({})],
}));
