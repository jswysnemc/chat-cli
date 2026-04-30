# Changelog

## 0.1.8 - 2026-05-01

- Fixed DeepSeek continuation requests by replaying saved reasoning text as `reasoning_content`.
- Added a provider/model patch gate for DeepSeek-style reasoning replay so other OpenAI-compatible providers keep their default message format.
- Added configuration and documentation for `replay_reasoning_content` provider/model patches.

## 0.1.7 - 2026-04-28

- Added interactive reasoning configuration support.
- Fixed native Anthropic tool calls, including thinking block replay and grouped tool results.
- Fixed OpenAI-compatible reasoning replay for tool calls so providers that require `reasoning_content` round-trips continue after tool execution.
- Added AUR `chat-cli-bin` maintenance automation for tagged releases.

## 0.1.6 - 2026-04-27

- Previous public release.
