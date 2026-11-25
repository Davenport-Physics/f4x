// f4x MIT shim; see REL/REL.h. Logging as the headers use it:
//   REX::ERROR("failed to find var \"{}\" on structure \"{}\"", a_name, name);
//   REX::FAIL("out of memory"sv);
//   REX::ELogLevel logLevel{ REX::ELogLevel::Info };
#pragma once

#include <cstdio>
#include <cstdlib>
#include <format>
#include <string>
#include <string_view>
#include <utility>

namespace REX
{
	enum class ELogLevel : std::uint32_t
	{
		Trace,
		Debug,
		Info,
		Warning,
		Error,
		Critical
	};

	namespace detail
	{
		template <class... Args>
		void log(ELogLevel a_level, std::string_view a_fmt, Args&&... a_args)
		{
			std::string text = std::vformat(a_fmt, std::make_format_args(a_args...));
			std::fprintf(stderr, "[%u] %s\n", static_cast<unsigned>(a_level), text.c_str());
		}
	}

	template <class... Args>
	void TRACE(std::string_view a_fmt, Args&&... a_args) { detail::log(ELogLevel::Trace, a_fmt, a_args...); }

	template <class... Args>
	void DEBUG(std::string_view a_fmt, Args&&... a_args) { detail::log(ELogLevel::Debug, a_fmt, a_args...); }

	template <class... Args>
	void INFO(std::string_view a_fmt, Args&&... a_args) { detail::log(ELogLevel::Info, a_fmt, a_args...); }

	template <class... Args>
	void WARN(std::string_view a_fmt, Args&&... a_args) { detail::log(ELogLevel::Warning, a_fmt, a_args...); }

	template <class... Args>
	void ERROR(std::string_view a_fmt, Args&&... a_args) { detail::log(ELogLevel::Error, a_fmt, a_args...); }

	template <class... Args>
	void CRITICAL(std::string_view a_fmt, Args&&... a_args) { detail::log(ELogLevel::Critical, a_fmt, a_args...); }

	// Logs and terminates.
	template <class... Args>
	[[noreturn]] void FAIL(std::string_view a_fmt, Args&&... a_args)
	{
		detail::log(ELogLevel::Critical, a_fmt, a_args...);
		std::abort();
	}
}
