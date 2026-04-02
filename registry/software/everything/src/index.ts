import coreutils from "@rivet-dev/agent-os-coreutils";
import sed from "@rivet-dev/agent-os-sed";
import grep from "@rivet-dev/agent-os-grep";
import gawk from "@rivet-dev/agent-os-gawk";
import findutils from "@rivet-dev/agent-os-findutils";
import diffutils from "@rivet-dev/agent-os-diffutils";
import tar from "@rivet-dev/agent-os-tar";
import gzip from "@rivet-dev/agent-os-gzip";
import curl from "@rivet-dev/agent-os-curl";
import zip from "@rivet-dev/agent-os-zip";
import unzip from "@rivet-dev/agent-os-unzip";
import jq from "@rivet-dev/agent-os-jq";
import ripgrep from "@rivet-dev/agent-os-ripgrep";
import fd from "@rivet-dev/agent-os-fd";
import tree from "@rivet-dev/agent-os-tree";
import file from "@rivet-dev/agent-os-file";
import yq from "@rivet-dev/agent-os-yq";
import codex from "@rivet-dev/agent-os-codex";
import pngcrush from "@rivet-dev/agent-os-pngcrush";

const everything = [
	coreutils,
	sed,
	grep,
	gawk,
	findutils,
	diffutils,
	tar,
	gzip,
	curl,
	zip,
	unzip,
	jq,
	ripgrep,
	fd,
	tree,
	file,
	yq,
	codex,
	pngcrush,
];

export default everything;
export {
	coreutils,
	sed,
	grep,
	gawk,
	findutils,
	diffutils,
	tar,
	gzip,
	curl,
	zip,
	unzip,
	jq,
	ripgrep,
	fd,
	tree,
	file,
	yq,
	codex,
	pngcrush,
};
