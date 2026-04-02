import common from "@rivet-dev/agent-os-common";
import make from "@rivet-dev/agent-os-make";
import git from "@rivet-dev/agent-os-git";
import curl from "@rivet-dev/agent-os-curl";

const buildEssential = [...common, make, git, curl];

export default buildEssential;
export { common, make, git, curl };
