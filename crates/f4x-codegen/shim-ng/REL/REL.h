// f4x MIT shim (not part of libxse/commonlib-shared). The next-gen CommonLibF4
// headers (libxse/commonlibf4 40a93f69) include <REL/REL.h> and <REX/REX.h> from a
// GPL submodule that f4x never uses. These shims provide exactly the names the
// headers use, written from that usage (and modelled on the MIT CommonLibF4
// 67173f8d `REL/Relocation.h` and `F4SE/Impl/PCH.h`), so clang can lay the
// engine records out. They are only ever compiled by `f4x-codegen dump --flavor ng`
// on the host; nothing here ends up in f4x.
#pragma once

#include <algorithm>
#include <array>
#include <atomic>
#include <bit>
#include <cassert>
#include <cmath>
#include <compare>
#include <concepts>
#include <cstdarg>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <cwchar>
#include <format>
#include <functional>
#include <initializer_list>
#include <iterator>
#include <limits>
#include <locale>
#include <map>
#include <memory>
#include <mutex>
#include <new>
#include <optional>
#include <ranges>
#include <set>
#include <source_location>
#include <span>
#include <sstream>
#include <stack>
#include <stdexcept>
#include <string>
#include <string_view>
#include <tuple>
#include <type_traits>
#include <typeinfo>
#include <unordered_map>
#include <utility>
#include <variant>
#include <vector>

#include "REL/Relocation.h"
#include "REL/Trampoline.h"
