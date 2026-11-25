// f4x MIT shim; see REL/REL.h. RE/T/TLS.h reads the thread's TLS array through
// `REX::W32::NtCurrentTeb()->threadLocalStoragePointer`.
#pragma once

#include "REX/W32/BASE.h"

namespace REX::W32
{
	// The documented head of the x64 TEB (winternl.h `TEB`: Reserved1[12], then the
	// PEB pointer at 0x60); the TLS array pointer is the 12th word (0x58).
	struct TEB
	{
		void* reserved[11];                // 00
		void* threadLocalStoragePointer;   // 58
		void* processEnvironmentBlock;     // 60
	};
	static_assert(offsetof(TEB, threadLocalStoragePointer) == 0x58);

	[[nodiscard]] TEB* NtCurrentTeb() noexcept;
}
