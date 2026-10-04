// Package model is the AI runtime's model gateway (ADR-0051): it speaks
// the OpenAI-compatible Chat Completions API with tool calls, which local
// model servers (Ollama, llama.cpp) and hosted providers offer alike.
package model

import (
	"encoding/json"
	"errors"
	"fmt"
)

// Message is one chat message.
type Message struct {
	Role       string     `json:"role"`
	Content    string     `json:"content"`
	ToolCalls  []ToolCall `json:"tool_calls,omitempty"`
	ToolCallID string     `json:"tool_call_id,omitempty"`
}

// ToolCall is the model asking for a tool to run.
type ToolCall struct {
	ID       string       `json:"id"`
	Type     string       `json:"type"`
	Function FunctionCall `json:"function"`
}

type FunctionCall struct {
	Name string `json:"name"`
	// Arguments is a JSON object, as text.
	Arguments string `json:"arguments"`
}

// Tool describes a tool to the model.
type Tool struct {
	Type     string   `json:"type"`
	Function Function `json:"function"`
}

type Function struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	Parameters  json.RawMessage `json:"parameters"`
}

// Model completes a conversation: the next assistant message.
type Model interface {
	Complete(messages []Message, tools []Tool) (Message, error)
}

// Poster sends a JSON body to an endpoint and returns the response
// status and body (the transport: Oceans TCP on Oceans, anything in tests).
type Poster interface {
	Post(path string, body []byte) (status int, response []byte, err error)
}

// Chat is a Chat Completions endpoint.
type Chat struct {
	Transport Poster
	// Path of the completions resource, e.g. "/v1/chat/completions".
	Path  string
	Model string
}

type request struct {
	Model    string    `json:"model"`
	Messages []Message `json:"messages"`
	Tools    []Tool    `json:"tools,omitempty"`
	Stream   bool      `json:"stream"`
}

type response struct {
	Choices []struct {
		Message Message `json:"message"`
	} `json:"choices"`
	Error *struct {
		Message string `json:"message"`
	} `json:"error"`
}

// ErrNoChoice: the reply held no message.
var ErrNoChoice = errors.New("the model returned no message")

// Complete implements Model.
func (c Chat) Complete(messages []Message, tools []Tool) (Message, error) {
	body, err := json.Marshal(request{Model: c.Model, Messages: messages, Tools: tools})
	if err != nil {
		return Message{}, err
	}
	status, data, err := c.Transport.Post(c.Path, body)
	if err != nil {
		return Message{}, err
	}
	var reply response
	if jsonErr := json.Unmarshal(data, &reply); jsonErr != nil {
		if status != 200 {
			return Message{}, fmt.Errorf("the model server answered HTTP %d", status)
		}
		return Message{}, fmt.Errorf("unreadable model reply: %w", jsonErr)
	}
	if reply.Error != nil {
		return Message{}, fmt.Errorf("the model server: %s", reply.Error.Message)
	}
	if status != 200 {
		return Message{}, fmt.Errorf("the model server answered HTTP %d", status)
	}
	if len(reply.Choices) == 0 {
		return Message{}, ErrNoChoice
	}
	return reply.Choices[0].Message, nil
}
