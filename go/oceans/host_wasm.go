//go:build wasip1

package oceans

import "unsafe"

// The host functions of the Oceans Go host (ADR-0050), module "oceans".
// Pointers are offsets into this module's linear memory; the host checks
// every range. Results >= 0 are values; < 0 are negated Oceans error codes.

//go:wasmimport oceans handle_find
func hostHandleFind(kind unsafe.Pointer, kindLen uint32, name unsafe.Pointer, nameLen uint32) int64

//go:wasmimport oceans args
func hostArgs(buf unsafe.Pointer, cap uint32) int64

//go:wasmimport oceans debug_write
func hostDebugWrite(log uint64, text unsafe.Pointer, n uint32) int64

//go:wasmimport oceans close
func hostClose(handle uint64) int64

//go:wasmimport oceans duplicate
func hostDuplicate(handle uint64, rights uint32) int64

//go:wasmimport oceans ipc_call
func hostCall(client uint64, label uint64, data unsafe.Pointer, dataLen uint32,
	handles unsafe.Pointer, handlesLen uint32, reply unsafe.Pointer, replyCap uint32,
	replyHandles unsafe.Pointer, replyHandlesCap uint32, result unsafe.Pointer) int64

//go:wasmimport oceans ipc_receive
func hostReceive(server uint64, data unsafe.Pointer, dataCap uint32,
	handles unsafe.Pointer, handlesCap uint32, result unsafe.Pointer) int64

//go:wasmimport oceans ipc_reply
func hostReply(label uint64, data unsafe.Pointer, dataLen uint32,
	handles unsafe.Pointer, handlesLen uint32) int64

//go:wasmimport oceans endpoint_mint
func hostMint(server uint64, badge uint64) int64

//go:wasmimport oceans notification_create
func hostNotificationCreate() int64

//go:wasmimport oceans notification_wait
func hostNotificationWait(notification uint64) int64

//go:wasmimport oceans endpoint_bind
func hostEndpointBind(server uint64, notification uint64) int64

//go:wasmimport oceans timer_set
func hostTimerSet(notification uint64, bits uint64, ms uint64) int64

//go:wasmimport oceans publish_text
func hostPublishText(text unsafe.Pointer, n uint32) int64

//go:wasmimport oceans system_info
func hostSystemInfo(sysinfo uint64, kind uint64, buf unsafe.Pointer, cap uint32) int64

func ptr(b []byte) unsafe.Pointer {
	if len(b) == 0 {
		return nil
	}
	return unsafe.Pointer(&b[0])
}

func handlesPtr(h []Handle) unsafe.Pointer {
	if len(h) == 0 {
		return nil
	}
	return unsafe.Pointer(&h[0])
}

func strPtr(s string) unsafe.Pointer {
	if len(s) == 0 {
		return nil
	}
	return unsafe.Pointer(unsafe.StringData(s))
}
