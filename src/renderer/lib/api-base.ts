/**
 * Shared API base URL cache for 21st.dev endpoints.
 * Used by both api-fetch.ts (TTS) and remote-api.ts (sandbox/chat data).
 */

let cachedBaseUrl: string | null = null

/**
 * Get the API base URL (cached after first call)
 * Always returns https://21st.dev (both in dev and production)
 */
export async function getApiBaseUrl(): Promise<string> {
  if (cachedBaseUrl) return cachedBaseUrl
  cachedBaseUrl = await window.desktopApi.getApiBaseUrl()
  return cachedBaseUrl
}

/**
 * Clear the cached base URL (useful for testing)
 */
export function clearApiBaseCache(): void {
  cachedBaseUrl = null
}
