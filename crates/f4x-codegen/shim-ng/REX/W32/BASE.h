// f4x MIT shim; see REL/REL.h. The Win32 names the next-gen CommonLibF4 headers use
// through REX::W32, declared without <windows.h> (whose macros would rename engine
// methods such as GetObject or PlaySound and change vtable slot names).
//
// Value types that appear in engine records follow the Windows SDK's public layouts;
// `f4x-codegen dump --flavor ng` checks each against the real SDK headers from xwin
// (shim-ng-check.cpp), so they cannot drift.
#pragma once

#include <cstddef>
#include <cstdint>

namespace REX::W32
{
	using BOOL = std::int32_t;
	using DWORD = std::uint32_t;
	using HANDLE = void*;
	using HMODULE = struct HINSTANCE__*;
	using HWND = struct HWND__*;

	inline constexpr std::uint32_t MAX_PATH{ 260 };
	inline constexpr std::uint32_t MEM_RELEASE{ 0x00008000 };

	// SDK `FILETIME` (minwindef.h): two 32-bit halves of a 100 ns tick count.
	struct FILETIME
	{
		std::uint32_t lo;  // 00 dwLowDateTime
		std::uint32_t hi;  // 04 dwHighDateTime
	};
	static_assert(sizeof(FILETIME) == 0x8);

	// SDK `CRITICAL_SECTION` = `RTL_CRITICAL_SECTION` (winnt.h).
	struct CRITICAL_SECTION
	{
		void*          debugInfo;       // 00
		std::int32_t   lockCount;       // 08
		std::int32_t   recursionCount;  // 0C
		HANDLE         owningThread;    // 10
		HANDLE         lockSemaphore;   // 18
		std::uintptr_t spinCount;       // 20
	};
	static_assert(sizeof(CRITICAL_SECTION) == 0x28);

	// kernel32 (declarations only; never called on the host).
	[[nodiscard]] HMODULE GetModuleHandleW(const wchar_t* a_name) noexcept;
	[[nodiscard]] HMODULE GetCurrentModule() noexcept;
	[[nodiscard]] void*   GetProcAddress(HMODULE a_module, const char* a_name) noexcept;
	[[nodiscard]] DWORD   GetCurrentThreadId() noexcept;
	[[nodiscard]] void*   TlsGetValue(DWORD a_index) noexcept;
	BOOL                  TlsSetValue(DWORD a_index, void* a_value) noexcept;
	BOOL                  VirtualFree(void* a_address, std::size_t a_size, DWORD a_type) noexcept;
	BOOL                  InitializeCriticalSectionAndSpinCount(CRITICAL_SECTION* a_section, DWORD a_spinCount) noexcept;
	void                  DeleteCriticalSection(CRITICAL_SECTION* a_section) noexcept;
	void                  EnterCriticalSection(CRITICAL_SECTION* a_section) noexcept;
	void                  LeaveCriticalSection(CRITICAL_SECTION* a_section) noexcept;
}
