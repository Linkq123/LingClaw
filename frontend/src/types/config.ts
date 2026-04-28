// TypeScript interfaces for the LingClaw JSON config shape

export interface ModelEntry {
  id: string;
  name?: string;
  reasoning?: boolean;
  input?: string[];
  contextWindow?: number;
  maxTokens?: number;
}

export interface ProviderConfig {
  api?: 'openai-completions' | 'anthropic' | 'ollama' | 'gemini';
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

export interface McpServerConfig {
  command?: string;
  args?: string[];
  cwd?: string;
  timeoutSecs?: number;
  enabled?: boolean;
  env?: Record<string, string>;
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
