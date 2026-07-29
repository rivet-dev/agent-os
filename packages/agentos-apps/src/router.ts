import { Hono } from "hono";
import { createClient } from "rivetkit/client";
import { AgentOSAppsError } from "./errors.js";
import { validateAppId } from "./source.js";

export interface AgentOSAppsRoutingClient {
	agentOSAppsApp: {
		getOrCreate(key?: string | string[]): {
			fetch(request: Request): Promise<Response>;
		};
	};
}

export interface CreateAppsRouterOptions {
	/** An ordinary RivetKit client. */
	client?: AgentOSAppsRoutingClient;
}

let defaultClient: AgentOSAppsRoutingClient | undefined;

function getDefaultClient(): AgentOSAppsRoutingClient {
	defaultClient ??= createClient() as unknown as AgentOSAppsRoutingClient;
	return defaultClient;
}

function errorResponse(error: unknown): Response {
	const code =
		error instanceof AgentOSAppsError
			? error.code
			: typeof error === "object" &&
					error !== null &&
					"code" in error &&
					typeof error.code === "string"
				? error.code
				: undefined;
	const status =
		code === "agentos_apps_invalid_app_id"
			? 400
			: code === "agentos_apps_not_deployed"
				? 404
				: code === "agentos_apps_region_not_deployed"
					? 404
					: code === "agentos_apps_request_limit"
						? 413
						: code?.startsWith("agentos_apps_")
							? 503
							: 500;
	return Response.json(
		{
			error: {
				code: code ?? "agentos_apps_internal_error",
				message:
					error instanceof Error
						? error.message
						: "AgentOS Apps request failed",
			},
		},
		{ status },
	);
}

export function createAppsRouter(options: CreateAppsRouterOptions = {}): Hono {
	const router = new Hono();
	const handler = async (context: {
		req: {
			param(name: string): string | undefined;
			path: string;
			routePath: string;
			raw: Request;
		};
	}) => {
		try {
			const appId = context.req.param("appId") ?? "";
			validateAppId(appId);
			const original = context.req.raw;
			const url = new URL(original.url);
			const parameterOffset = context.req.routePath.indexOf("/:appId");
			const mountPath =
				parameterOffset < 0
					? ""
					: context.req.routePath.slice(0, parameterOffset);
			const applicationPath = `${mountPath}/${appId}`;
			const suffix = context.req.path.startsWith(applicationPath)
				? context.req.path.slice(applicationPath.length)
				: "";
			if (suffix === "") {
				url.pathname = `${url.pathname}/`;
				return Response.redirect(url, 308);
			}
			url.pathname = suffix.startsWith("/") ? suffix : `/${suffix}`;
			const forwarded = new Request(url, original);
			return await (options.client ?? getDefaultClient()).agentOSAppsApp
				.getOrCreate([appId])
				.fetch(forwarded);
		} catch (error) {
			return errorResponse(error);
		}
	};
	router.all("/:appId", handler);
	router.all("/:appId/*", handler);
	return router;
}

export const appsRouter = createAppsRouter();

/** @internal Test-only reset for verifying lazy client creation. */
export function resetDefaultRouterClientForTest(): void {
	defaultClient = undefined;
}
