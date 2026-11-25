// f4x MIT shim; see REL/REL.h. RE/B/BSGraphics.h holds D3D11/DXGI interfaces by
// pointer (forward declarations suffice) and three DXGI value types by value, which
// follow the SDK's dxgicommon.h / dxgitype.h (checked against them by shim-ng-check.cpp).
#pragma once

#include "REX/W32/BASE.h"

namespace REX::W32
{
	struct ID3D11Buffer;
	struct ID3D11ComputeShader;
	struct ID3D11DepthStencilView;
	struct ID3D11Device;
	struct ID3D11DeviceContext;
	struct ID3D11DomainShader;
	struct ID3D11HullShader;
	struct ID3D11InputLayout;
	struct ID3D11PixelShader;
	struct ID3D11Query;
	struct ID3D11RenderTargetView;
	struct ID3D11ShaderResourceView;
	struct ID3D11Texture2D;
	struct ID3D11UnorderedAccessView;
	struct ID3D11VertexShader;
	struct IDXGISwapChain;

	struct DXGI_RATIONAL
	{
		std::uint32_t numerator;    // 00
		std::uint32_t denominator;  // 04
	};
	static_assert(sizeof(DXGI_RATIONAL) == 0x8);

	enum DXGI_MODE_SCANLINE_ORDER
	{
		DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED = 0,
		DXGI_MODE_SCANLINE_ORDER_PROGRESSIVE = 1,
		DXGI_MODE_SCANLINE_ORDER_UPPER_FIELD_FIRST = 2,
		DXGI_MODE_SCANLINE_ORDER_LOWER_FIELD_FIRST = 3
	};

	enum DXGI_MODE_SCALING
	{
		DXGI_MODE_SCALING_UNSPECIFIED = 0,
		DXGI_MODE_SCALING_CENTERED = 1,
		DXGI_MODE_SCALING_STRETCHED = 2
	};
}
