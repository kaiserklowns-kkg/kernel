//go:build !wasip1

package oceans

// Host builds (tests, tools) are not on Oceans: every call fails with
// ErrUnsupported, so pure logic built on this package still compiles and
// tests on the development machine.

func Find(kind, name string) (Handle, bool) { return 0, false }

func Args() string { return "" }

func DebugWrite(log Handle, text string) error { return ErrUnsupported }

func Close(h Handle) error { return ErrUnsupported }

func Duplicate(h Handle, rights uint32) (Handle, error) { return 0, ErrUnsupported }

func Call(client Handle, label uint64, data []byte, handles []Handle) (Message, error) {
	return Message{}, ErrUnsupported
}

func Receive(server Handle) (Message, error) { return Message{}, ErrUnsupported }

func Reply(label uint64, data []byte, handles []Handle) error { return ErrUnsupported }

func Mint(server Handle, badge uint64) (Handle, error) { return 0, ErrUnsupported }

func NotificationCreate() (Handle, error) { return 0, ErrUnsupported }

func NotificationWait(notification Handle) (uint64, error) { return 0, ErrUnsupported }

func EndpointBind(server, notification Handle) error { return ErrUnsupported }

func TimerSet(notification Handle, bits, ms uint64) error { return ErrUnsupported }

func PublishText(text string) (Handle, error) { return 0, ErrUnsupported }

func SystemInfo(sysinfo Handle, kind uint64) ([]byte, error) { return nil, ErrUnsupported }

func ReadText(memory Handle) (string, error) { return "", ErrUnsupported }

func MemorySize(memory Handle) (uint64, error) { return 0, ErrUnsupported }

func MemoryRead(memory Handle, offset uint64, buf []byte) (int, error) { return 0, ErrUnsupported }
