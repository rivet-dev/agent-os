#!/usr/bin/env node

import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve as resolvePath } from "node:path";

const require = createRequire(import.meta.url);
const sdkPath = require.resolve("@anthropic-ai/claude-agent-sdk");
const cliPath = resolvePath(dirname(sdkPath), "cli.js");
const distDir = resolvePath(import.meta.dirname, "..", "dist");
const manifestPath = resolvePath(distDir, "claude-cli-patched.json");
const sdkManifestPath = resolvePath(distDir, "claude-sdk-patched.json");

const source = readFileSync(cliPath, "utf-8");
const sdkSource = readFileSync(sdkPath, "utf-8");
const patches = [
	{
		name: "inject stdout normalization helpers",
		needle: 'import{createRequire as H15}from"node:module";',
		replacement:
			'import{createRequire as H15}from"node:module";if(process.env.CLAUDE_CODE_TRACE_STARTUP==="1")process.stderr.write("[agent-os-claude] bootstrap_loaded\\n");function __agentOsTrimOutput(q){if(typeof q==="string")return q.trim();if(q==null)return"";if(typeof q.trim==="function")return q.trim();if(typeof Buffer!=="undefined"&&Buffer.isBuffer(q))return q.toString("utf8").trim();if(q instanceof Uint8Array)return Buffer.from(q).toString("utf8").trim();return String(q).trim()}function __agentOsTrimStdout(q){return __agentOsTrimOutput(q?.stdout)}function __agentOsTrimStderr(q){return __agentOsTrimOutput(q?.stderr)}const __agentOsOriginalRealpath=globalThis["Nkq"];async function __agentOsRealpath(q){return typeof __agentOsOriginalRealpath==="function"?__agentOsOriginalRealpath(q):q}function __agentOsEnsureAsyncIterable(q){if(q&&typeof q[Symbol.asyncIterator]==="function")return q;if(q&&typeof q[Symbol.iterator]==="function")return(async function*(){for(let K of q)yield K})();if(q&&typeof q.on==="function"){return{[Symbol.asyncIterator](){let K=[],_=[],Y=!1,z=null;let A=(X)=>{K.push(X);O()};let O=()=>{for(;_.length>0;){if(z){_.shift().reject(z);continue}if(K.length>0){_.shift().resolve({done:!1,value:K.shift()});continue}if(Y){_.shift().resolve({done:!0,value:void 0});continue}break}};let $=()=>{Y=!0;w()};let w=()=>{q.off?.("data",A);q.off?.("end",$);q.off?.("close",$);q.off?.("error",j)};let j=(X)=>{z=X,Y=!0,w(),O()};q.on("data",A);q.on("end",$);q.on("close",$);q.on("error",j);q.resume?.();return{next(){if(z)return Promise.reject(z);if(K.length>0)return Promise.resolve({done:!1,value:K.shift()});if(Y)return Promise.resolve({done:!0,value:void 0});return new Promise((X,M)=>{_.push({resolve:X,reject:M})})},return(){Y=!0,w(),O();return Promise.resolve({done:!0,value:void 0})},[Symbol.asyncIterator](){return this}}}}}throw new TypeError("expected input to be async iterable")}',
	},
	{
		name: "ignore startup exit code in headless mode",
		needle:
			'if(process.exitCode!==void 0){k("Graceful shutdown initiated, skipping further initialization");return}',
		replacement:
			'if(process.exitCode!==void 0){if(process.env.CLAUDE_CODE_IGNORE_STARTUP_EXIT_CODE!=="1"){k("Graceful shutdown initiated, skipping further initialization");return}process.stderr.write("[agent-os-claude] ignoring_startup_exit_code "+String(process.exitCode)+"\\n");process.exitCode=void 0}',
	},
	{
		name: "guard EPIPE destroy calls for Agent OS stdio",
		needle:
			'function ow7(q){return(K)=>{if(K.code==="EPIPE")q.destroy()}}',
		replacement:
			'function ow7(q){return(K)=>{if(K.code==="EPIPE")typeof q.destroy=="function"?q.destroy():typeof q.end=="function"&&q.end()}}',
	},
	{
		name: "guard buffered stream destroy helper",
		needle:
			'Mr8=async(q,K)=>{if(!q||K===void 0)return;await Kz5(0),q.destroy();try{return await K}catch(_){return _.bufferedData}}',
		replacement:
			'Mr8=async(q,K)=>{if(!q||K===void 0)return;await Kz5(0),typeof q.destroy=="function"&&q.destroy();try{return await K}catch(_){return _.bufferedData}}',
	},
	{
		name: "force Agent OS ripgrep when requested",
		needle:
			'KG8=Y1(()=>{if(V_(process.env.USE_BUILTIN_RIPGREP)){let{cmd:Y}=x_8("rg",[]);if(Y!=="rg")return{mode:"system",command:"rg",args:[]}}if(lw())return{mode:"embedded",command:process.execPath,args:["--no-config"],argv0:"rg"};let K=Z16.resolve(dy_,"vendor","ripgrep");return{mode:"builtin",command:process.platform==="win32"?Z16.resolve(K,`${process.arch}-win32`,"rg.exe"):Z16.resolve(K,`${process.arch}-${process.platform}`,"rg"),args:[]}});',
		replacement:
			'KG8=Y1(()=>{if(process.env.CLAUDE_CODE_FORCE_AGENT_OS_RIPGREP==="1")return{mode:"system",command:"rg",args:[]};if(V_(process.env.USE_BUILTIN_RIPGREP)){let{cmd:Y}=x_8("rg",[]);if(Y!=="rg")return{mode:"system",command:"rg",args:[]}}if(lw())return{mode:"embedded",command:process.execPath,args:["--no-config"],argv0:"rg"};let K=Z16.resolve(dy_,"vendor","ripgrep");return{mode:"builtin",command:process.platform==="win32"?Z16.resolve(K,`${process.arch}-win32`,"rg.exe"):Z16.resolve(K,`${process.arch}-${process.platform}`,"rg"),args:[]}});',
	},
	{
		name: "guard missing events.setMaxListeners export",
		needle:
			'function C3(q=Xi_){let K=new AbortController;return Ji_(q,K.signal),K}',
		replacement:
			'function C3(q=Xi_){let K=new AbortController;return typeof Ji_=="function"&&Ji_(q,K.signal),K}',
	},
	{
		name: "fix shell snapshot login shell argument ordering",
		needle: 'O9Y(q,["-c","-l",j],{env:{',
		replacement: 'O9Y(q,["-l","-c",j],{env:{',
	},
	{
		name: "fix bash provider spawn argument ordering",
		needle:
			'return["-c",...O?[]:["-l"],A]},async getEnvironmentOverrides(A){',
		replacement:
			'return O?["-c",A]:["-l","-c",A]},async getEnvironmentOverrides(A){',
	},
	{
		name: "force pipe-mode bash output under Agent OS",
		needle:
			'L=await J.getEnvironmentOverrides(q),S=!!j,h=JL("local_bash"),x=new iA(h,A??null,!S);',
		replacement:
			'L=await J.getEnvironmentOverrides(q),S=!!j||process.env.CLAUDE_CODE_USE_PIPE_OUTPUT==="1",h=JL("local_bash"),x=new iA(h,A??null,!S);',
	},
	{
		name: "disable /dev/null shell redirection under Agent OS",
		needle: 'if(K)return Gq([q,"<","/dev/null"]);return Gq([q])',
		replacement:
			'if(process.env.CLAUDE_CODE_DISABLE_DEV_NULL_REDIRECT==="1")return Gq([q]);if(K)return Gq([q,"<","/dev/null"]);return Gq([q])',
	},
	{
		name: "disable cwd persistence checkpoint under Agent OS",
		needle: 'let Z=await lNq();if(Z)W.push(Z);let f=v9Y(q);if(f)W.push(f);W.push(`eval ${P}`),W.push(`pwd -P >| ${Gq([J])}`);let G=W.join(" && ");',
		replacement:
			'let Z=await lNq();if(Z)W.push(Z);let f=v9Y(q);if(f)W.push(f);W.push(`eval ${P}`),process.env.CLAUDE_CODE_DISABLE_CWD_PERSIST!=="1"&&W.push(`pwd -P >| ${Gq([J])}`);let G=W.join(" && ");',
	},
	{
		name: "use direct shell command execution under Agent OS",
		needle:
			'let G=W.join(" && ");if(process.env.CLAUDE_CODE_SHELL_PREFIX)G=rN8(process.env.CLAUDE_CODE_SHELL_PREFIX,G);',
		replacement:
			'let G=W.join(" && ");if(process.env.CLAUDE_CODE_SIMPLE_SHELL_EXEC==="1")G=A;if(process.env.CLAUDE_CODE_SHELL_PREFIX)G=rN8(process.env.CLAUDE_CODE_SHELL_PREFIX,G);',
	},
	{
		name: "trace bash shell spawn configuration",
		needle:
			'let V=G?"/bin/sh":f,N=G?["-c",W]:J.getSpawnArgs(W),L=await J.getEnvironmentOverrides(q),S=!!j||process.env.CLAUDE_CODE_USE_PIPE_OUTPUT==="1",h=JL("local_bash"),x=new iA(h,A??null,!S);',
		replacement:
			'let V=G?"/bin/sh":f,N=G?["-c",W]:J.getSpawnArgs(W),L=await J.getEnvironmentOverrides(q),S=!!j||process.env.CLAUDE_CODE_USE_PIPE_OUTPUT==="1";if(!G&&process.env.CLAUDE_CODE_FORCE_SH_FOR_BASH==="1")V="/bin/sh",N=["-c",W];if(process.env.CLAUDE_CODE_TRACE_BASH_SHELL==="1")process.stderr.write("[agent-os-claude] bash_spawn_config "+JSON.stringify({shell:V,args:N,cwd:Z,command:W,envOverrides:L,pipeOutput:S})+"\\n");let h=JL("local_bash"),x=new iA(h,A??null,!S);',
	},
	{
		name: "trace bash shell child process output",
		needle:
			'try{let p=h9Y(V,N,{env:{...Vu(),SHELL:_==="bash"?f:void 0,GIT_EDITOR:"true",CLAUDECODE:"1",...L,...{}},cwd:Z,stdio:S?["pipe","pipe","pipe"]:["pipe",I?.fd,I?.fd],detached:J.detached,windowsHide:!0}),B=aN8(p,K,H,x,w);',
		replacement:
			'try{let BA=Vu(),BB=Object.entries(BA).filter(([Q,i])=>typeof i!=="string"&&i!==void 0),BC=Object.fromEntries(Object.entries(BA).filter(([Q,i])=>typeof i==="string")),BD={...BC,SHELL:_==="bash"?f:void 0,GIT_EDITOR:"true",CLAUDECODE:"1",...L,...{}};if(process.env.CLAUDE_CODE_TRACE_DIRECT_XU==="1"){let Q=h9Y("xu",["hello-agent-os"],{env:BD,cwd:Z,stdio:["pipe","pipe","pipe"],detached:!1,windowsHide:!0}),i="",R="";Q.stdout?.on("data",(k)=>i+=String(k)),Q.stderr?.on("data",(k)=>R+=String(k));let se=await new Promise((k)=>Q.on("close",(ve,ye)=>k({code:ve,signal:ye})));process.stderr.write("[agent-os-claude] direct_xu "+JSON.stringify({stdout:i,stderr:R,...se})+"\\n")}let BE=V,BF=N,BG=BD;if(process.env.CLAUDE_CODE_NODE_SHELL_WRAPPER==="1"){let Q=\'const{spawn}=require("child_process");const cmd=process.env.CLAUDE_CODE_NODE_SHELL_COMMAND||"";const child=spawn(cmd,[],{cwd:process.env.CLAUDE_CODE_NODE_SHELL_CWD||process.cwd(),env:process.env,shell:true,stdio:["ignore","pipe","pipe"],windowsHide:true});child.stdout?.on("data",(c)=>process.stdout.write(c));child.stderr?.on("data",(c)=>process.stderr.write(c));child.on("close",(code)=>process.exit(typeof code==="number"?code:1));child.on("error",(error)=>{process.stderr.write(String(error?.stack??error)+"\\\\n");process.exit(126)});\';BE=process.execPath||"node",BF=["-e",Q],BG={...BD,CLAUDE_CODE_NODE_SHELL_COMMAND:W,CLAUDE_CODE_NODE_SHELL_CWD:Z};if(process.env.CLAUDE_CODE_TRACE_BASH_SHELL==="1")process.stderr.write("[agent-os-claude] bash_node_wrapper "+JSON.stringify({command:W,cwd:Z,exec:BE})+"\\n")}let p=h9Y(BE,BF,{env:BG,cwd:Z,stdio:S?["pipe","pipe","pipe"]:["pipe",I?.fd,I?.fd],detached:J.detached,windowsHide:!0});if(process.env.CLAUDE_CODE_TRACE_BASH_SHELL==="1")(BB.length>0&&process.stderr.write("[agent-os-claude] bash_non_string_env "+JSON.stringify(BB.map(([Q,i])=>[Q,typeof i]))+"\\n"),process.stderr.write("[agent-os-claude] bash_spawned "+JSON.stringify({pid:p.pid,shell:BE,args:BF})+"\\n"),p.stdout?.on("data",(Q)=>process.stderr.write("[agent-os-claude] bash_stdout "+JSON.stringify(String(Q))+"\\n")),p.stderr?.on("data",(Q)=>process.stderr.write("[agent-os-claude] bash_stderr "+JSON.stringify(String(Q))+"\\n")),p.on("exit",(Q,i)=>process.stderr.write("[agent-os-claude] bash_exit "+JSON.stringify({code:Q,signal:i})+"\\n")));let B=aN8(p,K,H,x,w);',
	},
	{
		name: "skip special CLI entrypoints under Agent OS",
		needle:
			'if(K("cli_entry"),process.argv[2]==="--claude-in-chrome-mcp"){',
		replacement:
			'let __agentOsArg2=process.argv[2];if(process.env.CLAUDE_CODE_TRACE_STARTUP==="1")process.stderr.write("[agent-os-claude] cli_argv "+JSON.stringify(process.argv)+"\\n");if(K("cli_entry"),process.env.CLAUDE_CODE_SKIP_SPECIAL_ENTRYPOINTS==="1"&&(__agentOsArg2==="--claude-in-chrome-mcp"||__agentOsArg2==="--chrome-native-host"||__agentOsArg2==="--computer-use-mcp"))process.stderr.write("[agent-os-claude] skip_special_entrypoint "+String(__agentOsArg2)+"\\n");else if(process.argv[2]==="--claude-in-chrome-mcp"){',
	},
	{
		name: "trace message loop startup",
		needle: 'n8("info","cli_message_loop_started");',
		replacement:
			'process.stderr.write("[agent-os-claude] cli_message_loop_started\\n"),n8("info","cli_message_loop_started");',
	},
	{
		name: "trace stdin message parsing",
		needle: 'if(z)n8("info","cli_stdin_message_parsed",{type:z.type}),yield z',
		replacement:
			'if(z)(process.stderr.write("[agent-os-claude] cli_stdin_message_parsed "+z.type+"\\n"),n8("info","cli_stdin_message_parsed",{type:z.type}),yield z)',
	},
	{
		name: "coerce structured IO input streams into async iterables",
		needle: "yield*K();for await(let _ of this.input)q+=_,yield*K();if(q){",
		replacement:
			"yield*K();for await(let _ of __agentOsEnsureAsyncIterable(this.input))q+=_,yield*K();if(q){",
	},
	{
		name: "trace initialize request handling",
		needle:
			'if(await Xcz(h6.request,h6.request_id,L6,f,_,I,q,!!H.enableAuthStatus,H,j,$),h6.request.promptSuggestions)',
		replacement:
			'if(process.stderr.write("[agent-os-claude] initialize_request_start\\n"),await Xcz(h6.request,h6.request_id,L6,f,_,I,q,!!H.enableAuthStatus,H,j,$),process.stderr.write("[agent-os-claude] initialize_request_done\\n"),h6.request.promptSuggestions)',
	},
	{
		name: "trace pre-runHeadlessStreaming bootstrap",
		needle:
			'aw7(),oJ("after_loadInitialMessages"),await jw8(),oJ("after_modelStrings");',
		replacement:
			'aw7(),oJ("after_loadInitialMessages"),process.stderr.write("[agent-os-claude] before_ensureModelStrings\\n"),await jw8(),process.stderr.write("[agent-os-claude] after_ensureModelStrings\\n"),oJ("after_modelStrings");',
	},
	{
		name: "trace before runHeadlessStreaming consumption",
		needle: 'oJ("before_runHeadlessStreaming");',
		replacement:
			'process.stderr.write("[agent-os-claude] before_runHeadlessStreaming\\n"),oJ("before_runHeadlessStreaming");',
	},
	{
		name: "trace runHeadless entry",
		needle: 'oJ("runHeadless_entry"),',
		replacement:
			'process.stderr.write("[agent-os-claude] runHeadless_entry\\n"),oJ("runHeadless_entry"),',
	},
	{
		name: "trace after grove check",
		needle: 'oJ("after_grove_check"),',
		replacement:
			'process.stderr.write("[agent-os-claude] after_grove_check\\n"),oJ("after_grove_check"),',
	},
	{
		name: "defer growthbook init when requested",
		needle:
			'process.stderr.write("[agent-os-claude] after_grove_check\\n"),oJ("after_grove_check"),Zi(),$.resumeSessionAt&&!$.resume){',
		replacement:
			'process.stderr.write("[agent-os-claude] after_grove_check\\n"),oJ("after_grove_check"),process.env.CLAUDE_CODE_DEFER_GROWTHBOOK_INIT==="1"?queueMicrotask(()=>{void Zi()}):Zi(),$.resumeSessionAt&&!$.resume){',
	},
	{
		name: "trace structured IO and stream-json guard setup",
		needle:
			'let w=Wcz(q,$);if($.outputFormat==="stream-json")VeK();let j=w7.getSandboxUnavailableReason();',
		replacement:
			'let w=Wcz(q,$);process.stderr.write("[agent-os-claude] after_structured_io\\n");if($.outputFormat==="stream-json")(process.stderr.write("[agent-os-claude] before_stream_json_guard\\n"),VeK(),process.stderr.write("[agent-os-claude] after_stream_json_guard\\n"));let j=w7.getSandboxUnavailableReason();process.stderr.write("[agent-os-claude] after_sandbox_reason\\n");',
	},
	{
		name: "allow skipping Claude sandbox initialization",
		needle:
			'else if(w7.isSandboxingEnabled())try{await w7.initialize(w.createSandboxAskCallback())}catch(x){process.stderr.write(`\n❌ Sandbox Error: ${i6(x)}\n`),iK(1,"other");return}',
		replacement:
			'else if(w7.isSandboxingEnabled())if(process.env.CLAUDE_CODE_SKIP_SANDBOX_INIT==="1")process.stderr.write("[agent-os-claude] sandbox_init_skipped\\n");else try{process.stderr.write("[agent-os-claude] before_sandbox_init\\n");await w7.initialize(w.createSandboxAskCallback());process.stderr.write("[agent-os-claude] after_sandbox_init\\n")}catch(x){process.stderr.write(`\n❌ Sandbox Error: ${i6(x)}\n`),iK(1,"other");return}',
	},
	{
		name: "disable stream-json hook event forwarding when requested",
		needle: 'if($.outputFormat==="stream-json"&&$.verbose)JMK((x)=>{',
		replacement:
			'if($.outputFormat==="stream-json"&&$.verbose&&false)JMK((x)=>{',
	},
	{
		name: "trace before loadInitialMessages",
		needle: 'if($.setupTrigger)await cI8($.setupTrigger);oJ("before_loadInitialMessages");',
		replacement:
			'process.stderr.write("[agent-os-claude] after_hook_event_registration\\n");if($.setupTrigger)(process.stderr.write("[agent-os-claude] before_setup_hooks\\n"),await cI8($.setupTrigger),process.stderr.write("[agent-os-claude] after_setup_hooks\\n"));process.stderr.write("[agent-os-claude] before_loadInitialMessages\\n"),oJ("before_loadInitialMessages");',
	},
	{
		name: "trace after loadInitialMessages returns",
		needle:
			'let H=K(),{messages:J,turnInterruptionState:X,agentSetting:M}=await Pcz(_,{continue:$.continue,teleport:$.teleport,resume:$.resume,resumeSessionAt:$.resumeSessionAt,forkSession:$.forkSession,outputFormat:$.outputFormat,sessionStartHooksPromise:$.sessionStartHooksPromise,restoredWorkerState:w.restoredWorkerState}),D=cYK();',
		replacement:
			'let H=K(),{messages:J,turnInterruptionState:X,agentSetting:M}=process.env.CLAUDE_CODE_SKIP_INITIAL_MESSAGES==="1"?(process.stderr.write("[agent-os-claude] skip_initial_messages\\n"),{messages:[],turnInterruptionState:void 0,agentSetting:void 0}):await Pcz(_,{continue:$.continue,teleport:$.teleport,resume:$.resume,resumeSessionAt:$.resumeSessionAt,forkSession:$.forkSession,outputFormat:$.outputFormat,sessionStartHooksPromise:$.sessionStartHooksPromise,restoredWorkerState:w.restoredWorkerState}),D=(process.stderr.write("[agent-os-claude] after_loadInitialMessages_return messages="+J.length+" exit="+String(process.exitCode)+" agent="+String(M)+"\\n"),cYK());',
	},
	{
		name: "trace prepend initial user message",
		needle: "if(D)w.prependUserMessage(D);",
		replacement:
			'if(D)(process.stderr.write("[agent-os-claude] prepend_initial_user_message\\n"),w.prependUserMessage(D));',
	},
	{
		name: "trace after agent restore block",
		needle:
			'if(!$.agent&&!NB()&&M){let{agentDefinition:x}=KJ6(M,void 0,{activeAgents:O,allAgents:O});if(x){if(_((I)=>({...I,agent:x.agentType})),!$.systemPrompt&&!Pw(x)){let I=x.getSystemPrompt();if(I)$.systemPrompt=I}$78(x.agentType)}}if(J.length===0&&process.exitCode!==void 0)return;',
		replacement:
			'if(!$.agent&&!NB()&&M){let{agentDefinition:x}=KJ6(M,void 0,{activeAgents:O,allAgents:O});if(x){if(_((I)=>({...I,agent:x.agentType})),!$.systemPrompt&&!Pw(x)){let I=x.getSystemPrompt();if(I)$.systemPrompt=I}$78(x.agentType)}}process.stderr.write("[agent-os-claude] after_agent_restore_block\\n");if(J.length===0&&process.exitCode!==void 0){if(process.env.CLAUDE_CODE_IGNORE_STARTUP_EXIT_CODE==="1"){process.stderr.write("[agent-os-claude] ignoring_post_initial_exit_code "+String(process.exitCode)+"\\n");process.exitCode=void 0}else return;}',
	},
	{
		name: "trace before tool filtering",
		needle: 'let Z=L68(H.mcp.tools,H.toolPermissionContext),',
		replacement:
			'process.stderr.write("[agent-os-claude] before_tool_filtering\\n");let Z=L68(H.mcp.tools,H.toolPermissionContext),',
	},
	{
		name: "trace after permission handler setup",
		needle:
			'if($.permissionPromptToolName)f=f.filter((x)=>!L_(x,$.permissionPromptToolName));aw7(),',
		replacement:
			'if($.permissionPromptToolName)f=f.filter((x)=>!L_(x,$.permissionPromptToolName));process.stderr.write("[agent-os-claude] after_permission_handler_setup\\n"),aw7(),',
	},
	{
		name: "trace after registerProcessOutputErrorHandlers",
		needle: 'aw7(),oJ("after_loadInitialMessages"),',
		replacement:
			'aw7(),process.stderr.write("[agent-os-claude] after_registerProcessOutputErrorHandlers\\n"),oJ("after_loadInitialMessages"),',
	},
	{
		name: "trace before connectMcp",
		needle: 'xq("before_connectMcp"),await z5(H3,"regular"),',
		replacement:
			'process.stderr.write("[agent-os-claude] before_connectMcp\\n"),xq("before_connectMcp"),await z5(H3,"regular"),',
	},
	{
		name: "trace after connectMcp",
		needle: 'xq("after_connectMcp"),await j6.then(',
		replacement:
			'process.stderr.write("[agent-os-claude] after_connectMcp\\n"),xq("after_connectMcp"),await j6.then(',
	},
	{
		name: "trace after claudeai MCP connect",
		needle: 'xq("after_connectMcp_claudeai"),',
		replacement:
			'process.stderr.write("[agent-os-claude] after_connectMcp_claudeai\\n"),xq("after_connectMcp_claudeai"),',
	},
	{
		name: "trace before print import",
		needle: 'S65(),xq("before_print_import");',
		replacement:
			'S65(),process.stderr.write("[agent-os-claude] before_print_import\\n"),xq("before_print_import");',
	},
	{
		name: "trace after print import",
		needle: 'xq("after_print_import"),OK(',
		replacement:
			'process.stderr.write("[agent-os-claude] after_print_import\\n"),xq("after_print_import"),OK(',
	},
	{
		name: "trace early input capture and main import",
		needle:
			'if(q.includes("--bare"))process.env.CLAUDE_CODE_SIMPLE="1";let{startCapturingEarlyInput:Y}=await Promise.resolve().then(() => (Jd6(),Dn4));Y(),K("cli_before_main_import");let{main:z}=await Promise.resolve().then(() => (eY7(),C65));K("cli_after_main_import"),await z(),K("cli_after_main_complete")',
		replacement:
			'if(q.includes("--bare"))process.env.CLAUDE_CODE_SIMPLE="1";process.stderr.write("[agent-os-claude] before_early_input_import\\n");let{startCapturingEarlyInput:Y}=await Promise.resolve().then(() => (Jd6(),Dn4));process.stderr.write("[agent-os-claude] after_early_input_import\\n");if(process.env.CLAUDE_CODE_SKIP_EARLY_INPUT_CAPTURE==="1")process.stderr.write("[agent-os-claude] skip_early_input_capture\\n");else{process.stderr.write("[agent-os-claude] before_early_input_start\\n");Y();process.stderr.write("[agent-os-claude] after_early_input_start\\n")}K("cli_before_main_import");process.stderr.write("[agent-os-claude] before_main_import\\n");let{main:z}=await Promise.resolve().then(() => (eY7(),C65));process.stderr.write("[agent-os-claude] after_main_import\\n");K("cli_after_main_import"),await z(),K("cli_after_main_complete")',
	},
];

let patched = source;
for (const patch of patches) {
	if (!patched.includes(patch.needle)) {
		throw new Error(`Could not find Claude CLI patch target: ${patch.name}`);
	}
	patched = patched.replace(patch.needle, patch.replacement);
}

patched = patched.replace(
	/\b([A-Za-z_$][\w$]*)\.stdout\.trim\(\)/g,
	"__agentOsTrimStdout($1)",
);
patched = patched.replace(
	/\b([A-Za-z_$][\w$]*)\.stderr\.trim\(\)/g,
	"__agentOsTrimStderr($1)",
);
patched = patched.replace(/\bNkq\(/g, "__agentOsRealpath(");

const sdkNeedle =
	'function y1($=AL){let X=new AbortController;return ML($,X.signal),X}';
const sdkReplacement =
	'function y1($=AL){let X=new AbortController;return typeof ML==="function"&&ML($,X.signal),X}';
const patchedSdk = sdkSource.includes(sdkNeedle)
	? sdkSource.replace(sdkNeedle, sdkReplacement)
	: sdkSource;

mkdirSync(distDir, { recursive: true });
const hash = createHash("sha256").update(patched).digest("hex").slice(0, 16);
const fileName = `claude-cli-patched-${hash}.mjs`;
const outputPath = resolvePath(distDir, fileName);
writeFileSync(outputPath, patched, "utf-8");

const sdkHash = createHash("sha256")
	.update(patchedSdk)
	.digest("hex")
	.slice(0, 16);
const sdkFileName = `claude-sdk-patched-${sdkHash}.mjs`;
const sdkOutputPath = resolvePath(distDir, sdkFileName);
writeFileSync(sdkOutputPath, patchedSdk, "utf-8");

writeFileSync(
	manifestPath,
	JSON.stringify({ entry: `./${fileName}` }, null, 2) + "\n",
	"utf-8",
);
writeFileSync(
	sdkManifestPath,
	JSON.stringify({ entry: `./${sdkFileName}` }, null, 2) + "\n",
	"utf-8",
);
process.stdout.write(`Wrote ${outputPath}\n`);
