import { PDFDocument } from "pdf-lib";
import mammoth from "mammoth";
import ExcelJS from "exceljs";
import JSZip from "jszip";

const pdf = await PDFDocument.create();
pdf.addPage([200, 100]).drawText("agentOS");
const pdfBytes = await pdf.save();
const loadedPdf = await PDFDocument.load(pdfBytes);

const docx = new JSZip();
docx.file("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>');
docx.folder("_rels").file(".rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>');
docx.folder("word").file("document.xml", '<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello agent</w:t></w:r></w:p></w:body></w:document>');
const docxBuffer = await docx.generateAsync({ type: "nodebuffer" });
const doc = await mammoth.extractRawText({ buffer: docxBuffer });

const workbook = new ExcelJS.Workbook();
const sheet = workbook.addWorksheet("Agents");
sheet.addRow(["name", "count"]);
sheet.addRow(["worker", 3]);
const xlsx = await workbook.xlsx.writeBuffer();
const reopened = new ExcelJS.Workbook();
await reopened.xlsx.load(xlsx);

console.log(JSON.stringify({
  pdfPages: loadedPdf.getPageCount(),
  docxText: doc.value.trim(),
  excelCell: reopened.getWorksheet("Agents").getCell("B2").value,
}));
