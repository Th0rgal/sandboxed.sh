/**
 * API Module Index
 * 
 * Re-exports all API functions and types for backward compatibility.
 * New code should import from specific modules when possible:
 * 
 * @example
 * // Preferred: Import from specific module
 * import { listMissions } from '@/lib/api/missions';
 * 
 * // Still works: Import from index
 * import { listMissions } from '@/lib/api';
 */

// Core utilities
export {
  apiUrl,
  isNetworkError,
  LibraryUnavailableError,
  apiFetch,
  apiGet,
  apiPost,
  apiPut,
  apiPatch,
  apiDel,
  libGet,
  libPost,
  libPut,
  libDel,
  ensureLibraryResponse,
} from "./core";

// Missions
export {
  type MissionStatus,
  type ModelEffort,
  type MissionHistoryEntry,
  type DesktopSessionInfo,
  type Mission,
  type StoredEvent,
  type CreateMissionOptions,
  type UpdateMissionSettingsOptions,
  type RunningMissionInfo,
  type MissionSearchResult,
  type MissionMomentSearchResult,
  listMissions,
  searchMissions,
  searchMissionMoments,
  getMission,
  getMissionEvents,
  getMissionEventsWithMeta,
  type MissionEventsMeta,
  getCurrentMission,
  createMission,
  loadMission,
  getRunningMissions,
  startMissionParallel,
  cancelMission,
  updateMissionSettings,
  setMissionStatus,
  deleteMission,
  cleanupEmptyMissions,
  resumeMission,
} from "./missions";

// Workspaces
export {
  type WorkspaceType,
  type WorkspaceStatus,
  type Workspace,
  type ContainerDistro,
  CONTAINER_DISTROS,
  type WorkspaceDebugInfo,
  type InitLogResponse,
  listWorkspaces,
  getWorkspace,
  createWorkspace,
  updateWorkspace,
  syncWorkspace,
  deleteWorkspace,
  buildWorkspace,
  getWorkspaceDebug,
  getWorkspaceInitLog,
} from "./workspaces";

// Providers
export {
  type AIProviderType,
  type AIProviderTypeInfo,
  type AIProviderStatus,
  type AIProviderAuthMethod,
  type AIProvider,
  type AIProviderAuthResponse,
  type OAuthAuthorizeResponse,
  type BackendProviderResponse,
  type ProviderModel,
  type Provider,
  type ProvidersResponse,
  type BackendModelOption,
  type BackendModelOptionsResponse,
  type CustomModel,
  listAIProviders,
  listAIProviderTypes,
  getAIProvider,
  createAIProvider,
  updateAIProvider,
  deleteAIProvider,
  getProviderForBackend,
  authenticateAIProvider,
  setDefaultAIProvider,
  getAuthMethods,
  oauthAuthorize,
  oauthCallback,
  listProviders,
  listBackendModelOptions,
  type ProviderUsage,
  getProviderUsage,
} from "./providers";

// Model Routing
export {
  type ChainEntry,
  type ModelChain,
  type ResolvedEntry,
  type AccountHealthSnapshot,
  type FallbackEvent,
  listModelChains,
  createModelChain,
  updateModelChain,
  deleteModelChain,
  resolveModelChain,
  listAccountHealth,
  clearAccountCooldown,
  listFallbackEvents,
} from "./model-routing";

// Automations
export {
  type CommandSource,
  type TriggerType,
  type Automation,
  type AutomationExecution,
  type ExecutionStatus,
  type CreateAutomationInput,
  listMissionAutomations,
  listActiveAutomations,
  createMissionAutomation,
  getAutomation,
  updateAutomation,
  updateAutomationActive,
  deleteAutomation,
  getAutomationExecutions,
  getMissionAutomationExecutions,
} from "./automations";

// Telegram
export {
  type TelegramTriggerMode,
  type TelegramChannel,
  type TelegramChatMission,
  type TelegramScheduledMessage,
  type TelegramStructuredMemoryScope,
  type TelegramStructuredMemoryEntry,
  type TelegramStructuredMemorySearchHit,
  type TelegramActionExecutionStatus,
  type TelegramActionExecutionKind,
  type TelegramActionExecution,
  type TelegramActionTarget,
  type TelegramActionExecutionResult,
  type CreateTelegramChannelInput,
  type CreateTelegramBotInput,
  type UpdateTelegramChannelInput,
  listTelegramChannels,
  createTelegramChannel,
  updateTelegramChannel,
  deleteTelegramChannel,
  listTelegramBots,
  createTelegramBot,
  listBotChats,
  listBotScheduledMessages,
  listBotStructuredMemory,
  searchBotStructuredMemory,
  listBotActionExecutions,
} from "./telegram";
