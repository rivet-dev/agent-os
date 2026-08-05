import { PDFDocument } from "pdf-lib";

try {
  const pdf = await PDFDocument.create();
  pdf.addPage([200, 100]);
  const pdfBytes = await pdf.save();
  const pdfjs = await import("pdfjs-dist/legacy/build/pdf.mjs");
  const loaded = await pdfjs.getDocument({ data: pdfBytes, disableWorker: true }).promise;
  console.log(loaded.numPages);
} catch (error) {
  console.error("NATIVE_ADDON_UNSUPPORTED: " + (error?.message ?? String(error)));
  process.exit(1);
}
