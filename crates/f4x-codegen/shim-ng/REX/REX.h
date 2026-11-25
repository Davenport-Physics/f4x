// f4x MIT shim; see REL/REL.h. The REX helpers the next-gen headers use. Only
// TEnum and TEnumSet appear in engine records; each stores exactly one `U`, like the
// MIT CommonLibF4's `stl::enumeration<E, U>` they replace (asserted below).
#pragma once

#include <atomic>
#include <concepts>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <type_traits>
#include <utility>

#include "REL/Relocation.h"
#include "REX/LOG.h"
#include "REX/W32/BASE.h"

namespace REX
{
	// An enum stored as `U` (a plain value: `get`, `underlying`, assignment).
	template <class E, class U = std::underlying_type_t<E>>
	class TEnum
	{
	public:
		using enum_type = E;
		using underlying_type = U;

		static_assert(std::is_enum_v<E>, "E must be an enum");
		static_assert(std::is_integral_v<U>, "U must be an integral");

		constexpr TEnum() noexcept = default;
		constexpr TEnum(const TEnum&) noexcept = default;
		constexpr TEnum(TEnum&&) noexcept = default;

		template <class U2>
		constexpr TEnum(TEnum<E, U2> a_rhs) noexcept :
			_impl(static_cast<U>(a_rhs.get()))
		{}

		constexpr TEnum(E a_value) noexcept :
			_impl(static_cast<U>(a_value))
		{}

		constexpr TEnum& operator=(const TEnum&) noexcept = default;
		constexpr TEnum& operator=(TEnum&&) noexcept = default;

		template <class U2>
		constexpr TEnum& operator=(TEnum<E, U2> a_rhs) noexcept
		{
			_impl = static_cast<U>(a_rhs.get());
			return *this;
		}

		constexpr TEnum& operator=(E a_value) noexcept
		{
			_impl = static_cast<U>(a_value);
			return *this;
		}

		[[nodiscard]] explicit constexpr operator bool() const noexcept { return _impl != static_cast<U>(0); }

		[[nodiscard]] constexpr E operator*() const noexcept { return get(); }
		[[nodiscard]] constexpr E get() const noexcept { return static_cast<E>(_impl); }
		[[nodiscard]] constexpr U underlying() const noexcept { return _impl; }

		[[nodiscard]] friend constexpr bool operator==(TEnum a_lhs, TEnum a_rhs) noexcept { return a_lhs._impl == a_rhs._impl; }
		[[nodiscard]] friend constexpr bool operator==(TEnum a_lhs, E a_rhs) noexcept { return a_lhs.get() == a_rhs; }
		[[nodiscard]] friend constexpr auto operator<=>(TEnum a_lhs, TEnum a_rhs) noexcept { return a_lhs._impl <=> a_rhs._impl; }
		[[nodiscard]] friend constexpr auto operator<=>(TEnum a_lhs, E a_rhs) noexcept { return a_lhs._impl <=> static_cast<U>(a_rhs); }

	private:
		U _impl{ 0 };
	};

	// A set of enum flags stored as `U` (`set`, `reset`, `any`, `all`, `none`).
	template <class E, class U = std::underlying_type_t<E>>
	class TEnumSet
	{
	public:
		using enum_type = E;
		using underlying_type = U;

		static_assert(std::is_enum_v<E>, "E must be an enum");
		static_assert(std::is_integral_v<U>, "U must be an integral");

		constexpr TEnumSet() noexcept = default;
		constexpr TEnumSet(const TEnumSet&) noexcept = default;
		constexpr TEnumSet(TEnumSet&&) noexcept = default;

		template <class U2>
		constexpr TEnumSet(TEnumSet<E, U2> a_rhs) noexcept :
			_impl(static_cast<U>(a_rhs.get()))
		{}

		template <class... Args>
		constexpr TEnumSet(Args... a_values) noexcept  //
			requires(sizeof...(Args) > 0 && (std::same_as<Args, E> && ...))
			:
			_impl((static_cast<U>(a_values) | ...))
		{}

		constexpr TEnumSet& operator=(const TEnumSet&) noexcept = default;
		constexpr TEnumSet& operator=(TEnumSet&&) noexcept = default;

		template <class U2>
		constexpr TEnumSet& operator=(TEnumSet<E, U2> a_rhs) noexcept
		{
			_impl = static_cast<U>(a_rhs.get());
			return *this;
		}

		constexpr TEnumSet& operator=(E a_value) noexcept
		{
			_impl = static_cast<U>(a_value);
			return *this;
		}

		[[nodiscard]] explicit constexpr operator bool() const noexcept { return _impl != static_cast<U>(0); }

		[[nodiscard]] constexpr E operator*() const noexcept { return get(); }
		[[nodiscard]] constexpr E get() const noexcept { return static_cast<E>(_impl); }
		[[nodiscard]] constexpr U underlying() const noexcept { return _impl; }

		template <class... Args>
		constexpr TEnumSet& set(Args... a_args) noexcept  //
			requires(std::same_as<Args, E> && ...)
		{
			_impl |= (static_cast<U>(a_args) | ...);
			return *this;
		}

		template <class... Args>
		constexpr TEnumSet& set(bool a_set, Args... a_args) noexcept  //
			requires(std::same_as<Args, E> && ...)
		{
			return a_set ? set(a_args...) : reset(a_args...);
		}

		constexpr TEnumSet& reset() noexcept
		{
			_impl = 0;
			return *this;
		}

		template <class... Args>
		constexpr TEnumSet& reset(Args... a_args) noexcept  //
			requires(sizeof...(Args) > 0 && (std::same_as<Args, E> && ...))
		{
			_impl &= ~(static_cast<U>(a_args) | ...);
			return *this;
		}

		template <class... Args>
		[[nodiscard]] constexpr bool any(Args... a_args) const noexcept  //
			requires(std::same_as<Args, E> && ...)
		{
			return (_impl & (static_cast<U>(a_args) | ...)) != static_cast<U>(0);
		}

		template <class... Args>
		[[nodiscard]] constexpr bool all(Args... a_args) const noexcept  //
			requires(std::same_as<Args, E> && ...)
		{
			return (_impl & (static_cast<U>(a_args) | ...)) == (static_cast<U>(a_args) | ...);
		}

		template <class... Args>
		[[nodiscard]] constexpr bool none(Args... a_args) const noexcept  //
			requires(std::same_as<Args, E> && ...)
		{
			return (_impl & (static_cast<U>(a_args) | ...)) == static_cast<U>(0);
		}

		[[nodiscard]] friend constexpr bool operator==(TEnumSet a_lhs, TEnumSet a_rhs) noexcept { return a_lhs._impl == a_rhs._impl; }
		[[nodiscard]] friend constexpr bool operator==(TEnumSet a_lhs, E a_rhs) noexcept { return a_lhs.get() == a_rhs; }
		[[nodiscard]] friend constexpr auto operator<=>(TEnumSet a_lhs, TEnumSet a_rhs) noexcept { return a_lhs._impl <=> a_rhs._impl; }
		[[nodiscard]] friend constexpr auto operator<=>(TEnumSet a_lhs, E a_rhs) noexcept { return a_lhs._impl <=> static_cast<U>(a_rhs); }

		[[nodiscard]] friend constexpr TEnumSet operator&(TEnumSet a_lhs, E a_rhs) noexcept { return static_cast<E>(a_lhs._impl & static_cast<U>(a_rhs)); }
		[[nodiscard]] friend constexpr TEnumSet operator|(TEnumSet a_lhs, E a_rhs) noexcept { return static_cast<E>(a_lhs._impl | static_cast<U>(a_rhs)); }
		[[nodiscard]] friend constexpr TEnumSet operator^(TEnumSet a_lhs, E a_rhs) noexcept { return static_cast<E>(a_lhs._impl ^ static_cast<U>(a_rhs)); }
		[[nodiscard]] friend constexpr TEnumSet operator&(TEnumSet a_lhs, TEnumSet a_rhs) noexcept { return static_cast<E>(a_lhs._impl & a_rhs._impl); }
		[[nodiscard]] friend constexpr TEnumSet operator|(TEnumSet a_lhs, TEnumSet a_rhs) noexcept { return static_cast<E>(a_lhs._impl | a_rhs._impl); }
		[[nodiscard]] friend constexpr TEnumSet operator^(TEnumSet a_lhs, TEnumSet a_rhs) noexcept { return static_cast<E>(a_lhs._impl ^ a_rhs._impl); }

		constexpr TEnumSet& operator&=(E a_rhs) noexcept { _impl &= static_cast<U>(a_rhs); return *this; }
		constexpr TEnumSet& operator|=(E a_rhs) noexcept { _impl |= static_cast<U>(a_rhs); return *this; }
		constexpr TEnumSet& operator^=(E a_rhs) noexcept { _impl ^= static_cast<U>(a_rhs); return *this; }

		// Arithmetic on the stored value (BSScript::TypeInfo moves between the array
		// and element ranges of RawType with `+=` / `-=`).
		constexpr TEnumSet& operator+=(E a_rhs) noexcept { _impl = static_cast<U>(_impl + static_cast<U>(a_rhs)); return *this; }
		constexpr TEnumSet& operator-=(E a_rhs) noexcept { _impl = static_cast<U>(_impl - static_cast<U>(a_rhs)); return *this; }
		constexpr TEnumSet& operator+=(U a_rhs) noexcept { _impl = static_cast<U>(_impl + a_rhs); return *this; }
		constexpr TEnumSet& operator-=(U a_rhs) noexcept { _impl = static_cast<U>(_impl - a_rhs); return *this; }

	private:
		U _impl{ 0 };
	};

	template <class... Args>
	TEnumSet(Args...) -> TEnumSet<std::common_type_t<Args...>, std::underlying_type_t<std::common_type_t<Args...>>>;

	namespace detail
	{
		enum class probe_enum : std::uint8_t
		{
		};
	}
	static_assert(sizeof(TEnum<detail::probe_enum, std::uint32_t>) == 4);
	static_assert(sizeof(TEnumSet<detail::probe_enum, std::uint16_t>) == 2);
	static_assert(alignof(TEnumSet<detail::probe_enum, std::uint64_t>) == 8);

	// An atomic view of a plain (possibly volatile or mutable) integer member:
	//   REX::TAtomicRef myRefCount{ refCount }; return ++myRefCount;
	template <class T>
	class TAtomicRef
	{
	public:
		using value_type = std::remove_cv_t<T>;

		explicit TAtomicRef(T& a_value) noexcept :
			_impl(const_cast<value_type&>(a_value))
		{}

		value_type operator++() noexcept { return ++_impl; }
		value_type operator++(int) noexcept { return _impl++; }
		value_type operator--() noexcept { return --_impl; }
		value_type operator--(int) noexcept { return _impl--; }

		value_type operator+=(value_type a_value) noexcept { return _impl += a_value; }
		value_type operator-=(value_type a_value) noexcept { return _impl -= a_value; }

		operator value_type() const noexcept { return _impl.load(); }

		[[nodiscard]] value_type load() const noexcept { return _impl.load(); }
		void                     store(value_type a_value) noexcept { _impl.store(a_value); }
		value_type               exchange(value_type a_value) noexcept { return _impl.exchange(a_value); }
		value_type               fetch_add(value_type a_value) noexcept { return _impl.fetch_add(a_value); }
		value_type               fetch_sub(value_type a_value) noexcept { return _impl.fetch_sub(a_value); }

		bool compare_exchange_weak(value_type& a_expected, value_type a_desired) noexcept
		{
			return _impl.compare_exchange_weak(a_expected, a_desired);
		}

		bool compare_exchange_strong(value_type& a_expected, value_type a_desired) noexcept
		{
			return _impl.compare_exchange_strong(a_expected, a_desired);
		}

	private:
		std::atomic_ref<value_type> _impl;
	};

	template <class T>
	TAtomicRef(T&) -> TAtomicRef<T>;

	// A string usable as a template argument: TStaticString<CharT, N> holds N
	// characters plus a terminator.
	template <class CharT, std::size_t N>
	struct TStaticString
	{
		using char_type = CharT;
		using size_type = std::size_t;

		constexpr TStaticString(const CharT (&a_string)[N + 1]) noexcept
		{
			for (size_type i = 0; i < N; ++i) {
				c[i] = a_string[i];
			}
		}

		[[nodiscard]] constexpr const CharT* data() const noexcept { return c; }
		[[nodiscard]] constexpr size_type    length() const noexcept { return N; }
		[[nodiscard]] constexpr size_type    size() const noexcept { return N; }

		CharT c[N + 1]{};
	};

	template <class CharT, std::size_t N>
	TStaticString(const CharT (&)[N]) -> TStaticString<CharT, N - 1>;

	// Runs a callable when the scope ends: const REX::TScopeExit d{ [&]() noexcept { ... } };
	template <class F>
	class TScopeExit
	{
	public:
		explicit TScopeExit(F a_fn) noexcept(std::is_nothrow_move_constructible_v<F>) :
			_fn(std::move(a_fn))
		{}

		TScopeExit(const TScopeExit&) = delete;
		TScopeExit& operator=(const TScopeExit&) = delete;

		~TScopeExit() noexcept
		{
			if (_active) {
				_fn();
			}
		}

		void release() noexcept { _active = false; }

	private:
		F    _fn;
		bool _active{ true };
	};

	template <class F>
	TScopeExit(F) -> TScopeExit<F>;

	// Writes T's primary game vtable into an object built by our own constructor:
	//   hkReferencedObject() { REX::EMPLACE_VTABLE(this); }
	template <class T>
	void EMPLACE_VTABLE(T* a_ptr)
	{
		reinterpret_cast<std::uintptr_t*>(a_ptr)[0] = T::VTABLE[0].address();
	}
}
