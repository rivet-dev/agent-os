/** @type {import('next').NextConfig} */
module.exports = {
	outputFileTracing: false,
	eslint: {
		ignoreDuringBuilds: true,
	},
	experimental: {
		parallelServerBuildTraces: false,
		parallelServerCompiles: false,
		webpackBuildWorker: false,
	},
	typescript: {
		ignoreBuildErrors: true,
	},
};
