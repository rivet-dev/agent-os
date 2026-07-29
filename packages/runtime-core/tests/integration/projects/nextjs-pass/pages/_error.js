const React = require("react");

function ErrorPage(props) {
	return React.createElement(
		"div",
		null,
		"Error " + String(props.statusCode || 500),
	);
}

ErrorPage.getInitialProps = function getInitialProps(context) {
	return {
		statusCode: context.res
			? context.res.statusCode
			: context.err
				? context.err.statusCode
				: 404,
	};
};

module.exports = ErrorPage;
