const React = require("react");

function Home() {
	return React.createElement("div", null, "Hello from Next.js");
}

module.exports = Home;

module.exports.getServerSideProps = async function getServerSideProps() {
	return { props: {} };
};
