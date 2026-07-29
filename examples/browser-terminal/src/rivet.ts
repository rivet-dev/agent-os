import { createRivetKit } from "@rivetkit/react";
import type { registry } from "../server";

const ENDPOINT =
	(import.meta.env.VITE_AGENTOS_ENDPOINT as string | undefined) ??
	"http://localhost:6420";

export const { useActor } = createRivetKit<typeof registry>(ENDPOINT);

export const ACTOR_NAME = "shellVm";
