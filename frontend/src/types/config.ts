// TypeScript interfaces for the LingClaw JSON config shape

export interface ModelCompat {
  thinkingFormat?: string;
  [key: string]: unknown;
}

export interface ModelEntry {
  id: string;
  name?: string;
  reasoning?: boolean;
  input?: string[];
  contextWindow?: number;
  maxTokens?: number;
  compat?: ModelCompat;
}

export interface ProviderConfig {
  api?: 'openai-completions' | 'openai-responses' | 'anthropic' | 'ollama' | 'gemini';
  baseUrl?: string;
  apiKey?: string;
  models?: ModelEntry[];
}

export interface SettingsConfig {
  port?: number;
  execTimeout?: number;
  toolTimeout?: number;
  subAgentTimeout?: number;
  maxLlmRetries?: number;
  maxContextTokens?: number;
  maxOutputBytes?: number;
  maxFileBytes?: number;
  structuredMemory?: boolean;
  dailyReflection?: boolean;
  enableStateDigest?: boolean;
  enableS3?: boolean;
  openaiStreamIncludeUsage?: boolean;
  anthropicPromptCaching?: boolean;
}

export interface AgentModelDefaults {
  primary?: string;
  fast?: string;
  'sub-agent'?: string;
  memory?: string;
  reflection?: string;
  context?: string;
  [key: string]: string | undefined;
}

export interface DiscoveredAgentInfo {
  name: string;
  description?: string;
  source?: string;
}

export interface SessionSkillInfo {
  id: string;
  name: string;
  description?: string;
  path: string;
  group?: string;
  enabled: boolean;
}

export interface SessionSkillsApiResponse {
  ok?: boolean;
  session?: {
    id: string;
    name?: string;
  };
  skills?: SessionSkillInfo[];
  enabledSystemSkills?: string[];
  disabledSystemSkills?: string[];
  error?: string;
}

export interface McpServerConfig {
  transport?: 'stdio' | 'streamable-http';
  command?: string;
  url?: string;
  args?: string[];
  cwd?: string;
  timeoutSecs?: number;
  enabled?: boolean;
  env?: Record<string, string>;
  headers?: Record<string, string>;
  auth?: {
    clientId?: string;
    clientSecret?: string;
    scopes?: string[];
  };
}

export interface McpCatalogServer {
  id: string;
  name: string;
  transport: string;
  configuredEnabled: boolean;
  enabled: boolean;
  authenticated?: boolean;
  toolCount?: number;
  resourceCount?: number;
  promptCount?: number;
  error?: string;
}

export interface McpCatalogTool {
  id: string;
  server: string;
  rawName: string;
  name: string;
  description?: string;
  readOnly?: boolean;
  enabled: boolean;
}

export interface McpCatalogResource {
  server: string;
  uri: string;
  name: string;
  description?: string;
  mimeType?: string;
}

export interface McpCatalogPrompt {
  server: string;
  name: string;
  description?: string;
  arguments?: unknown;
}

export interface McpSessionPolicy {
  enabledServers?: string[];
  enabledTools?: string[];
  confirmMutatingTools?: boolean;
  clientCapabilities?: {
    roots?: boolean;
    sampling?: boolean;
    elicitation?: boolean;
  };
}

export interface McpCatalogResponse {
  session?: {
    id: string;
    name?: string;
  };
  policy?: McpSessionPolicy;
  servers?: McpCatalogServer[];
  tools?: McpCatalogTool[];
  resources?: McpCatalogResource[];
  prompts?: McpCatalogPrompt[];
  error?: string;
}

export interface S3Config {
  endpoint?: string;
  region?: string;
  bucket?: string;
  accessKey?: string;
  secretKey?: string;
  prefix?: string;
  urlExpirySecs?: number;
  lifecycleDays?: number;
}

export interface AppConfig {
  settings?: SettingsConfig;
  models?: {
    providers?: Record<string, ProviderConfig>;
  };
  agents?: {
    defaults?: {
      model?: AgentModelDefaults;
    };
  };
  mcpServers?: Record<string, McpServerConfig>;
  s3?: S3Config;
}

export interface ConfigApiResponse {
  config?: AppConfig;
  path?: string;
  parse_error?: string;
  raw?: string;
  error?: string;
  line?: number;
  column?: number;
  discoveredAgents?: DiscoveredAgentInfo[];
}

export interface UsageData {
  daily_input?: number;
  daily_output?: number;
  total_input?: number;
  total_output?: number;
  input_source?: string;
  output_source?: string;
  source_scope?: string;
  daily_roles?: Record<string, [number, number]>;
  total_roles?: Record<string, [number, number]>;
  usage_history?: Array<{
    date: string;
    input: number;
    output: number;
    providers?: Record<string, [number, number]>;
  }>;
  daily_providers?: Record<string, [number, number]>;
}
