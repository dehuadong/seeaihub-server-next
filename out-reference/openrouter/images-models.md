
## openai/gpt-image-2

```json
{
  "id": "openai/gpt-image-2",
  "name": "OpenAI: GPT Image 2",
  "description": "OpenAI's latest image generation model. Supports high-fidelity image generation and editing via the dedicated Images API.",
  "architecture": {
    "input_modalities": [
	  "text",
	  "image"
	],
	"output_modalities": [
	  "image"
	]
  },
  "provider_name": "OpenAI",
  "provider_slug": "openai",
  "provider_tag": "openai",
  "supported_parameters": {
       "aspect_ratio": {
          "type": "enum",
          "values": [
            "1:1",
            "3:2",
            "2:3",
            "4:3",
            "3:4",
            "16:9",
            "9:16",
            "21:9",
            "auto"
          ]
        },
        "quality": {
          "type": "enum",
          "values": [
            "auto",
            "low",
            "medium",
            "high"
          ]
        },
        "background": {
          "type": "enum",
          "values": [
            "auto",
            "opaque"
          ]
        },
        "n": {
          "type": "range",
          "min": 1,
          "max": 10
        },
        "input_references": {
          "type": "range",
          "min": 0,
          "max": 16
        },
        "output_compression": {
          "type": "range",
          "min": 0,
          "max": 100
  },
  "allowed_passthrough_parameters": [
	"moderation"
  ],
  "supports_streaming": true,
  "pricing": [
        {
          "billable": "input_image",
          "unit": "token",
          "cost_usd": 0.000008
        },
        {
          "billable": "input_text",
          "unit": "token",
          "cost_usd": 0.000005
        },
        {
          "billable": "output_image",
          "unit": "token",
          "cost_usd": 0.00003
        }
  ]
   
}
```


## gemini-3.1-flash-lite-image

```json

{
  "id": "google/gemini-3.1-flash-lite-image",
  "architecture": {
	"input_modalities": [
	  "image",
	  "text"
	],
	"output_modalities": [
	  "image",
	  "text"
	]
  },
  "provider_name": "Google Vertex",
  "provider_slug": "google-vertex/global",
  "provider_tag": "google-vertex/global",
  "supported_parameters": {
	"resolution": {
	  "type": "enum",
	  "values": [
		"1K"
	  ]
	},
	"aspect_ratio": {
	  "type": "enum",
	  "values": [
		"1:1",
		"1:4",
		"1:8",
		"2:3",
		"3:2",
		"3:4",
		"4:1",
		"4:3",
		"4:5",
		"5:4",
		"8:1",
		"9:16",
		"16:9",
		"21:9"
	  ]
	},
	"n": {
	  "type": "range",
	  "min": 1,
	  "max": 1
	},
	"input_references": {
	  "type": "range",
	  "min": 0,
	  "max": 14
	}
  },
  "allowed_passthrough_parameters": [
	"cachedContent"
  ],
  "supports_streaming": false,
  "pricing": [
	{
	  "billable": "output_image",
	  "unit": "token",
	  "cost_usd": 0.00003
	}
  ]

}
```

## gemini-3.1-flash-image

```json

{
  "id": "google/gemini-3.1-flash-image",
  "architecture": {
	"input_modalities": [
	  "image",
	  "text"
	],
	"output_modalities": [
	  "image",
	  "text"
	]
  },

  "provider_name": "Google Vertex",
  "provider_slug": "google-vertex/global",
  "provider_tag": "google-vertex/global",
  "supported_parameters": {
	"resolution": {
	  "type": "enum",
	  "values": [
		"512",
		"1K",
		"2K",
		"4K"
	  ]
	},
	"aspect_ratio": {
	  "type": "enum",
	  "values": [
		"1:1",
		"1:4",
		"1:8",
		"2:3",
		"3:2",
		"3:4",
		"4:1",
		"4:3",
		"4:5",
		"5:4",
		"8:1",
		"9:16",
		"16:9",
		"21:9"
	  ]
	},
	"n": {
	  "type": "range",
	  "min": 1,
	  "max": 1
	},
	"input_references": {
	  "type": "range",
	  "min": 0,
	  "max": 14
	}
  },
  "allowed_passthrough_parameters": [
	"cachedContent"
  ],
  "supports_streaming": false,
  "pricing": [
	{
	  "billable": "output_image",
	  "unit": "token",
	  "cost_usd": 0.00006
	}
  ]

}

```

## gemini-3-pro-image

```json
    {
      "id": "google/gemini-3-pro-image",
      "name": "Google: Nano Banana Pro (Gemini 3 Pro Image)",
      "description": "Nano Banana Pro is Google’s most advanced image-generation and editing model, built on Gemini 3 Pro. It extends the original Nano Banana with significantly improved multimodal reasoning, real-world grounding, and...",
      "architecture": {
        "input_modalities": [
          "image",
          "text"
        ],
        "output_modalities": [
          "image",
          "text"
        ]
      },
  "provider_name": "Google Vertex",
  "provider_slug": "google-vertex/global",
  "provider_tag": "google-vertex/global",
      "supported_parameters": {
        "resolution": {
          "type": "enum",
          "values": [
            "1K",
            "2K",
            "4K"
          ]
        },
        "aspect_ratio": {
          "type": "enum",
          "values": [
            "1:1",
            "2:3",
            "3:2",
            "3:4",
            "4:3",
            "4:5",
            "5:4",
            "9:16",
            "16:9",
            "21:9"
          ]
        },
        "n": {
          "type": "range",
          "min": 1,
          "max": 1
        },
        "input_references": {
          "type": "range",
          "min": 0,
          "max": 14
        }
      },
      "allowed_passthrough_parameters": [
        "cachedContent"
      ],
      "supports_streaming": false,
      "pricing": [
        {
          "billable": "input_image",
          "unit": "token",
          "cost_usd": 0.000002
        },
        {
          "billable": "output_image",
          "unit": "token",
          "cost_usd": 0.00012
        }
      ]
    }
```


>google/gemini-3.1-flash-lite-image、google/gemini-3.1-flash-image、google/gemini-3-pro-image，还在支持的供应渠道：google-ai-studio

```
"provider_name": "Google AI Studio",
"provider_slug": "google-ai-studio",
"provider_tag": "google-ai-studio",
...其他参数同上（google-vertex/global）

```
