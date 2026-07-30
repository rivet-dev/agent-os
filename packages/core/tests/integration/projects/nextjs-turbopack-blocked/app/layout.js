const React = require("react");

module.exports = function RootLayout({ children }) {
  return React.createElement("html", null, React.createElement("body", null, children));
};
