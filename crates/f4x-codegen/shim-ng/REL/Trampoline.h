// f4x MIT shim; see REL/REL.h. The headers only name REL::Trampoline (F4SE/Trampoline.h
// aliases it and forwards REL::GetTrampoline), so a declaration-level class suffices.
#pragma once

#include <cstddef>
#include <cstdint>

namespace REL
{
	// A code buffer near the game image for branch/call thunks.
	class Trampoline
	{
	public:
		Trampoline() noexcept = default;

		void create(std::size_t a_size, void* a_module = nullptr);

		[[nodiscard]] void* allocate(std::size_t a_size);

		template <std::size_t N>
		std::uintptr_t write_branch(std::uintptr_t a_src, std::uintptr_t a_dst);

		template <std::size_t N>
		std::uintptr_t write_call(std::uintptr_t a_src, std::uintptr_t a_dst);

		[[nodiscard]] std::size_t empty() const noexcept { return _capacity - _size; }
		[[nodiscard]] std::size_t capacity() const noexcept { return _capacity; }

	private:
		std::byte*  _data{ nullptr };
		std::size_t _capacity{ 0 };
		std::size_t _size{ 0 };
	};

	[[nodiscard]] Trampoline& GetTrampoline() noexcept;
}
