// f4x MIT shim; see REL/REL.h. REL::ID, REL::Offset, REL::Version and
// REL::Relocation<T> as the next-gen CommonLibF4 headers use them:
//   inline constexpr REL::ID X{ 123 };            std::array<REL::ID, N> VTABLE{ ... };
//   REL::Relocation<func_t> func{ ID::X };        return func(this, a_arg);
//   REL::Relocation<T**> singleton{ ID::X };      return *singleton;
//   REL::Relocation<T*> map{ ID::X, -0x8 };       REL::Relocation<T*>{ REL::Offset(rva) }.get();
//   REL::Relocation<std::uintptr_t> vt{ ID::X };  vt.address();
//   REL::Version{ 1, 11, 221, 0 }, .pack(), Version::unpack(u32)
// None of these types is a member of an engine record, so their own layout does not
// affect the generated tables; ID and Offset still hold one integer each.
#pragma once

#include <array>
#include <compare>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <functional>
#include <memory>
#include <string>
#include <string_view>
#include <type_traits>
#include <utility>

#include "REX/W32/BASE.h"

namespace REL
{
	namespace detail
	{
		// Image base of the running module (never called on the host).
		[[nodiscard]] inline std::uintptr_t image_base() noexcept
		{
			return reinterpret_cast<std::uintptr_t>(REX::W32::GetModuleHandleW(nullptr));
		}

		// Resolves an Address Library ID to an RVA (never called on the host).
		[[nodiscard]] std::size_t id_to_offset(std::uint64_t a_id);
	}

	// An Address Library ID.
	class ID
	{
	public:
		constexpr ID() noexcept = default;

		explicit constexpr ID(std::uint64_t a_id) noexcept :
			_id(a_id)
		{}

		[[nodiscard]] constexpr std::uint64_t id() const noexcept { return _id; }
		[[nodiscard]] std::size_t             offset() const { return detail::id_to_offset(_id); }
		[[nodiscard]] std::uintptr_t          address() const { return detail::image_base() + offset(); }

	private:
		std::uint64_t _id{ 0 };
	};

	// An RVA in the game's image.
	class Offset
	{
	public:
		constexpr Offset() noexcept = default;

		explicit constexpr Offset(std::size_t a_offset) noexcept :
			_offset(a_offset)
		{}

		[[nodiscard]] constexpr std::size_t offset() const noexcept { return _offset; }
		[[nodiscard]] std::uintptr_t        address() const { return detail::image_base() + _offset; }

	private:
		std::size_t _offset{ 0 };
	};

	// major.minor.patch.build; packs as F4SE's 0xMMmmPPPB plugin version word
	// (the inverse of F4SE::QueryInterface::MakeVersion in F4SE/Interfaces.h).
	class Version
	{
	public:
		using value_type = std::uint16_t;

		constexpr Version() noexcept = default;

		constexpr Version(value_type a_v1, value_type a_v2 = 0, value_type a_v3 = 0, value_type a_v4 = 0) noexcept :
			_impl{ a_v1, a_v2, a_v3, a_v4 }
		{}

		[[nodiscard]] constexpr value_type&       operator[](std::size_t a_idx) noexcept { return _impl[a_idx]; }
		[[nodiscard]] constexpr const value_type& operator[](std::size_t a_idx) const noexcept { return _impl[a_idx]; }

		[[nodiscard]] constexpr value_type major() const noexcept { return _impl[0]; }
		[[nodiscard]] constexpr value_type minor() const noexcept { return _impl[1]; }
		[[nodiscard]] constexpr value_type patch() const noexcept { return _impl[2]; }
		[[nodiscard]] constexpr value_type build() const noexcept { return _impl[3]; }

		[[nodiscard]] constexpr std::uint32_t pack() const noexcept
		{
			return static_cast<std::uint32_t>(
				((_impl[0] & 0x0FFu) << 24u) |
				((_impl[1] & 0x0FFu) << 16u) |
				((_impl[2] & 0xFFFu) << 4u) |
				((_impl[3] & 0x00Fu) << 0u));
		}

		[[nodiscard]] static constexpr Version unpack(std::uint32_t a_packed) noexcept
		{
			return Version{
				static_cast<value_type>((a_packed >> 24) & 0x0FF),
				static_cast<value_type>((a_packed >> 16) & 0x0FF),
				static_cast<value_type>((a_packed >> 4) & 0xFFF),
				static_cast<value_type>((a_packed >> 0) & 0x00F)
			};
		}

		[[nodiscard]] std::string string(std::string_view a_separator = ".") const
		{
			std::string result;
			for (std::size_t i = 0; i < _impl.size(); ++i) {
				if (i != 0) {
					result += a_separator;
				}
				result += std::to_string(_impl[i]);
			}
			return result;
		}

		[[nodiscard]] friend constexpr bool                 operator==(const Version&, const Version&) noexcept = default;
		[[nodiscard]] friend constexpr std::strong_ordering operator<=>(const Version&, const Version&) noexcept = default;

	private:
		std::array<value_type, 4> _impl{ 0, 0, 0, 0 };
	};

	template <class T>
	class Relocation
	{
	public:
		using value_type = std::conditional_t<
			std::is_member_pointer_v<T> || std::is_function_v<std::remove_pointer_t<T>>,
			std::decay_t<T>,
			T>;

		constexpr Relocation() noexcept = default;

		explicit constexpr Relocation(std::uintptr_t a_address) noexcept :
			_impl(a_address)
		{}

		explicit Relocation(ID a_id) :
			_impl(a_id.address())
		{}

		explicit Relocation(ID a_id, std::ptrdiff_t a_offset) :
			_impl(a_id.address() + a_offset)
		{}

		explicit Relocation(Offset a_offset) :
			_impl(a_offset.address())
		{}

		[[nodiscard]] constexpr std::uintptr_t address() const noexcept { return _impl; }
		[[nodiscard]] std::size_t              offset() const { return _impl - detail::image_base(); }

		[[nodiscard]] value_type get() const  //
			requires(std::is_copy_constructible_v<value_type>)
		{
			if constexpr (std::is_member_pointer_v<value_type>) {
				value_type result{};
				std::memcpy(std::addressof(result), std::addressof(_impl), sizeof(_impl));
				return result;
			} else if constexpr (std::is_pointer_v<value_type>) {
				return reinterpret_cast<value_type>(_impl);
			} else {
				// A plain object at the address (e.g. `REL::Relocation<float> gravity`).
				return *reinterpret_cast<const value_type*>(_impl);
			}
		}

		template <class U = value_type>
		[[nodiscard]] decltype(auto) operator*() const noexcept  //
			requires(std::is_pointer_v<U>)
		{
			return *get();
		}

		template <class U = value_type>
		[[nodiscard]] auto operator->() const noexcept  //
			requires(std::is_pointer_v<U>)
		{
			return get();
		}

		// Calls the function at the address. A member function pointer is called as a
		// free function taking `this` first (the x64 convention; results that are not
		// x64 PODs are not needed by the headers' inline code).
		template <class... Args>
		decltype(auto) operator()(Args&&... a_args) const  //
			requires(std::is_member_function_pointer_v<value_type> || std::is_pointer_v<value_type>)
		{
			if constexpr (std::is_member_function_pointer_v<value_type>) {
				return std::invoke(get(), std::forward<Args>(a_args)...);
			} else {
				return get()(std::forward<Args>(a_args)...);
			}
		}

	private:
		std::uintptr_t _impl{ 0 };
	};
}
