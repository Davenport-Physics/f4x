// f4x MIT. Compiled (syntax only) by `f4x-codegen dump --flavor ng` as its `shim-check`
// pass: every Win32 value type our REX::W32 shims give the engine records must match
// the Windows SDK's own definition (from xwin) in size, alignment and field offsets.
//
// The shims come first: <windows.h> defines macros (MAX_PATH, MEM_RELEASE, ERROR, ...)
// that would rewrite the shims' names, so their constants are captured before it.
#include "REL/REL.h"
#include "REX/REX.h"
#include "REX/W32/D3D11.h"
#include "REX/W32/NT.h"

namespace shim
{
	inline constexpr auto max_path = REX::W32::MAX_PATH;
	inline constexpr auto mem_release = REX::W32::MEM_RELEASE;
	inline constexpr auto scaling_stretched = REX::W32::DXGI_MODE_SCALING_STRETCHED;
	inline constexpr auto scanline_lower_first = REX::W32::DXGI_MODE_SCANLINE_ORDER_LOWER_FIELD_FIRST;
	using FILETIME = REX::W32::FILETIME;
	using CRITICAL_SECTION = REX::W32::CRITICAL_SECTION;
	using DXGI_RATIONAL = REX::W32::DXGI_RATIONAL;
	using DXGI_MODE_SCALING = REX::W32::DXGI_MODE_SCALING;
	using DXGI_MODE_SCANLINE_ORDER = REX::W32::DXGI_MODE_SCANLINE_ORDER;
	using HWND = REX::W32::HWND;
	using TEB = REX::W32::TEB;
}

#include <windows.h>
#include <d3d11.h>
#include <winternl.h>

#define F4X_SAME_LAYOUT(a_shim, a_sdk)                     \
	static_assert(sizeof(a_shim) == sizeof(a_sdk));        \
	static_assert(alignof(a_shim) == alignof(a_sdk))

#define F4X_SAME_OFFSET(a_shim, a_field, a_sdk, a_sdkField) \
	static_assert(offsetof(a_shim, a_field) == offsetof(a_sdk, a_sdkField))

static_assert(shim::max_path == MAX_PATH);
static_assert(shim::mem_release == MEM_RELEASE);

F4X_SAME_LAYOUT(shim::FILETIME, ::FILETIME);
F4X_SAME_OFFSET(shim::FILETIME, lo, ::FILETIME, dwLowDateTime);
F4X_SAME_OFFSET(shim::FILETIME, hi, ::FILETIME, dwHighDateTime);

F4X_SAME_LAYOUT(shim::CRITICAL_SECTION, ::CRITICAL_SECTION);
F4X_SAME_OFFSET(shim::CRITICAL_SECTION, debugInfo, ::CRITICAL_SECTION, DebugInfo);
F4X_SAME_OFFSET(shim::CRITICAL_SECTION, lockCount, ::CRITICAL_SECTION, LockCount);
F4X_SAME_OFFSET(shim::CRITICAL_SECTION, recursionCount, ::CRITICAL_SECTION, RecursionCount);
F4X_SAME_OFFSET(shim::CRITICAL_SECTION, owningThread, ::CRITICAL_SECTION, OwningThread);
F4X_SAME_OFFSET(shim::CRITICAL_SECTION, lockSemaphore, ::CRITICAL_SECTION, LockSemaphore);
F4X_SAME_OFFSET(shim::CRITICAL_SECTION, spinCount, ::CRITICAL_SECTION, SpinCount);

F4X_SAME_LAYOUT(shim::DXGI_RATIONAL, ::DXGI_RATIONAL);
F4X_SAME_OFFSET(shim::DXGI_RATIONAL, numerator, ::DXGI_RATIONAL, Numerator);
F4X_SAME_OFFSET(shim::DXGI_RATIONAL, denominator, ::DXGI_RATIONAL, Denominator);

F4X_SAME_LAYOUT(shim::DXGI_MODE_SCALING, ::DXGI_MODE_SCALING);
F4X_SAME_LAYOUT(shim::DXGI_MODE_SCANLINE_ORDER, ::DXGI_MODE_SCANLINE_ORDER);
static_assert(static_cast<int>(shim::scaling_stretched) == DXGI_MODE_SCALING_STRETCHED);
static_assert(static_cast<int>(shim::scanline_lower_first) == DXGI_MODE_SCANLINE_ORDER_LOWER_FIELD_FIRST);

F4X_SAME_LAYOUT(shim::HWND, ::HWND);

// winternl.h leaves the TLS array pointer unnamed (Reserved1[11]).
F4X_SAME_OFFSET(shim::TEB, processEnvironmentBlock, ::TEB, ProcessEnvironmentBlock);
static_assert(offsetof(shim::TEB, threadLocalStoragePointer) == offsetof(::TEB, Reserved1) + 11 * sizeof(PVOID));
