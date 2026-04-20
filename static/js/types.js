/**
 * @typedef {Object} ImageAttachment
 * @property {string} url
 * @property {string} [object_key]
 * @property {string} [attachment_token]
 */

/**
 * @typedef {Object} HistoryMessage
 * @property {string} role - 'user' | 'assistant' | 'tool_call' | 'tool_result'
 * @property {string} content
 * @property {ImageAttachment[]} [images]
 * @property {string} [id]
 * @property {number} [timestamp]
 * @property {string} [name]
 * @property {string} [arguments]
 * @property {string} [result]
 */

/**
 * @typedef {'analyze' | 'act' | 'observe' | 'finish' | ''} ReactPhase
 */

/**
 * @typedef {Object} SessionMessage
 * @property {'session'} type
 * @property {string} id
 * @property {string} [name]
 * @property {{ image?: boolean, s3?: boolean }} [capabilities]
 * @property {boolean} [show_tools]
 * @property {boolean} [show_reasoning]
 */

/**
 * @typedef {Object} HistoryEvent
 * @property {'history'} type
 * @property {HistoryMessage[]} [messages]
 */

/**
 * @typedef {Object} DeltaEvent
 * @property {'delta'} type
 * @property {string} content
 */

/**
 * @typedef {Object} ToolCallEvent
 * @property {'tool_call'} type
 * @property {string} name
 * @property {string} arguments
 * @property {string} id
 */

/**
 * @typedef {Object} ToolProgressEvent
 * @property {'tool_progress'} type
 * @property {string} id
 * @property {string} [name]
 * @property {number} [elapsed_ms]
 */

/**
 * @typedef {Object} ToolResultEvent
 * @property {'tool_result'} type
 * @property {string} name
 * @property {string} [result]
 * @property {string} id
 * @property {number} [duration_ms]
 * @property {boolean} [is_error]
 * @property {string} [subagent]
 * @property {string} [task_id]
 */

/**
 * @typedef {Object} TaskEvent
 * @property {'task_started' | 'task_progress' | 'task_tool' | 'task_completed' | 'task_failed'} type
 * @property {string} agent
 * @property {string} [task_id]
 * @property {string} [prompt]
 * @property {number} [cycle]
 * @property {string} [tool]
 * @property {string} [arguments]
 * @property {number} [cycles]
 * @property {number} [tool_calls]
 * @property {number} [duration_ms]
 * @property {number} [input_tokens]
 * @property {number} [output_tokens]
 * @property {string} [error]
 * @property {string} [result_preview]
 * @property {string} [result_excerpt]
 */

/**
 * @typedef {Object} SystemEvent
 * @property {'system' | 'success' | 'error' | 'progress'} type
 * @property {string} content
 */

/**
 * @typedef {Object} ReactPhaseEvent
 * @property {'react_phase'} type
 * @property {ReactPhase} phase
 * @property {number} cycle
 */

/**
 * @typedef {SessionMessage | HistoryEvent | DeltaEvent | ToolCallEvent | ToolProgressEvent | ToolResultEvent | TaskEvent | SystemEvent | ReactPhaseEvent | { type: 'start' | 'done' | 'view_state' | 'thinking_start' | 'thinking_delta' | 'thinking_done' | 'context_compressed' }} WebSocketMessage
 */
