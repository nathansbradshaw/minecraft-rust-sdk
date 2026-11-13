//! Reviewed and sanitized recorded failure; no credentials or live targets.
use super::Result;
use serde_json::{Value, json};
pub const CAPTURE: &str = r###"
{
  "format": "bot-dev-menu-trace-v1",
  "provenance": {
    "case": "2026-11-13 E provider-recovery continuation, final read-only span",
    "source_revision": "fb8ffdbdc02614cfcf014ef63895bdb122deb553",
    "raw_journal_sha256": "bb4028b3e683c6c1bd24ed6e29873c9f1b4e53f3268bc5863b37b9377c2cfe94",
    "scope": "Sanitized recorded tool inputs, elapsed model durations and menu pages; player identity, position, credentials, reasoning and host paths excluded. Pages deduplicated by offset; expanded observations retain original revisions. Offline replay is not inference or native gameplay."
  },
  "pages": [
    {
      "menu": {
        "carried": {
          "count": 3,
          "item": "minecraft:birch_planks"
        },
        "menu_id": 1,
        "slots": [
          {
            "count": 1,
            "inventory_slot": 0,
            "item": "minecraft:birch_button",
            "may_pickup": true,
            "may_place_carried": false,
            "player_inventory": false,
            "slot": 0
          },
          {
            "count": 1,
            "inventory_slot": 0,
            "item": "minecraft:birch_planks",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 1
          },
          {
            "count": 0,
            "inventory_slot": 1,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 2
          },
          {
            "count": 0,
            "inventory_slot": 2,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 3
          },
          {
            "count": 0,
            "inventory_slot": 3,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 4
          },
          {
            "count": 0,
            "inventory_slot": 4,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 5
          },
          {
            "count": 0,
            "inventory_slot": 5,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 6
          },
          {
            "count": 0,
            "inventory_slot": 6,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 7
          },
          {
            "count": 0,
            "inventory_slot": 7,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 8
          },
          {
            "count": 0,
            "inventory_slot": 8,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": false,
            "slot": 9
          },
          {
            "count": 0,
            "inventory_slot": 9,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 10
          },
          {
            "count": 0,
            "inventory_slot": 10,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 11
          },
          {
            "count": 0,
            "inventory_slot": 11,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 12
          },
          {
            "count": 0,
            "inventory_slot": 12,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 13
          },
          {
            "count": 0,
            "inventory_slot": 13,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 14
          },
          {
            "count": 0,
            "inventory_slot": 14,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 15
          },
          {
            "count": 0,
            "inventory_slot": 15,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 16
          },
          {
            "count": 0,
            "inventory_slot": 16,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 17
          },
          {
            "count": 0,
            "inventory_slot": 17,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 18
          },
          {
            "count": 0,
            "inventory_slot": 18,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 19
          },
          {
            "count": 0,
            "inventory_slot": 19,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 20
          },
          {
            "count": 0,
            "inventory_slot": 20,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 21
          },
          {
            "count": 0,
            "inventory_slot": 21,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 22
          },
          {
            "count": 0,
            "inventory_slot": 22,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 23
          },
          {
            "count": 0,
            "inventory_slot": 23,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 24
          },
          {
            "count": 0,
            "inventory_slot": 24,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 25
          },
          {
            "count": 0,
            "inventory_slot": 25,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 26
          },
          {
            "count": 0,
            "inventory_slot": 26,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 27
          },
          {
            "count": 0,
            "inventory_slot": 27,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 28
          },
          {
            "count": 0,
            "inventory_slot": 28,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 29
          },
          {
            "count": 0,
            "inventory_slot": 29,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 30
          },
          {
            "count": 0,
            "inventory_slot": 30,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 31
          }
        ],
        "state_id": 10,
        "summary": {
          "empty": false,
          "items": {
            "minecraft:birch_button": 1,
            "minecraft:birch_planks": 1
          },
          "non_player_slots": 10,
          "occupied_non_player_slots": 2,
          "omitted_item_types": 0,
          "scope": "complete received SDK menu snapshot; non-player slots only",
          "total_slots": 46
        },
        "type": "minecraft:crafting"
      },
      "page": {
        "binding": "Each page is live, not an atomic multi-page snapshot. Match stable entity IDs or slot numbers; restart menu paging if menu/state ID changes. Complete describes this snapshot suffix only; omitted records are unknown.",
        "complete": false,
        "next_offset": 32,
        "offset": 0,
        "returned": 32,
        "total_in_snapshot": 46
      },
      "player": {
        "dimension": "minecraft:overworld",
        "position": [
          -1390.4997889091146,
          64.0,
          -1767.4997880056314
        ],
        "uuid": "69778480-0ba5-421a-8236-81c15fdbaeed"
      },
      "revision": "244100",
      "view": "menu",
      "world": {}
    },
    {
      "menu": {
        "carried": {
          "count": 3,
          "item": "minecraft:birch_planks"
        },
        "menu_id": 1,
        "slots": [
          {
            "count": 0,
            "inventory_slot": 31,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 32
          },
          {
            "count": 0,
            "inventory_slot": 32,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 33
          },
          {
            "count": 0,
            "inventory_slot": 33,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 34
          },
          {
            "count": 0,
            "inventory_slot": 34,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 35
          },
          {
            "count": 0,
            "inventory_slot": 35,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 36
          },
          {
            "count": 2,
            "inventory_slot": 0,
            "item": "minecraft:birch_planks",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 37
          },
          {
            "count": 0,
            "inventory_slot": 1,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 38
          },
          {
            "count": 4,
            "inventory_slot": 2,
            "item": "minecraft:stick",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 39
          },
          {
            "count": 0,
            "inventory_slot": 3,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 40
          },
          {
            "count": 0,
            "inventory_slot": 4,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 41
          },
          {
            "count": 0,
            "inventory_slot": 5,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 42
          },
          {
            "count": 0,
            "inventory_slot": 6,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 43
          },
          {
            "count": 0,
            "inventory_slot": 7,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 44
          },
          {
            "count": 0,
            "inventory_slot": 8,
            "item": "minecraft:air",
            "may_pickup": true,
            "may_place_carried": true,
            "player_inventory": true,
            "slot": 45
          }
        ],
        "state_id": 10,
        "summary": {
          "empty": false,
          "items": {
            "minecraft:birch_button": 1,
            "minecraft:birch_planks": 1
          },
          "non_player_slots": 10,
          "occupied_non_player_slots": 2,
          "omitted_item_types": 0,
          "scope": "complete received SDK menu snapshot; non-player slots only",
          "total_slots": 46
        },
        "type": "minecraft:crafting"
      },
      "page": {
        "binding": "Each page is live, not an atomic multi-page snapshot. Match stable entity IDs or slot numbers; restart menu paging if menu/state ID changes. Complete describes this snapshot suffix only; omitted records are unknown.",
        "complete": true,
        "next_offset": null,
        "offset": 32,
        "returned": 14,
        "total_in_snapshot": 46
      },
      "player": {
        "dimension": "minecraft:overworld",
        "position": [
          -1390.4997889091146,
          64.0,
          -1767.4997880056314
        ],
        "uuid": "69778480-0ba5-421a-8236-81c15fdbaeed"
      },
      "revision": "244430",
      "view": "menu",
      "world": {}
    }
  ],
  "steps": [
    {
      "sequence": 360,
      "elapsed_ms": 14213,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "244100",
      "offset": 0
    },
    {
      "sequence": 363,
      "elapsed_ms": 16457,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":32,\"limit\":14}",
      "revision": "244430",
      "offset": 32
    },
    {
      "sequence": 366,
      "elapsed_ms": 10578,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "244645",
      "offset": 0
    },
    {
      "sequence": 369,
      "elapsed_ms": 13963,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":32,\"limit\":14}",
      "revision": "244925",
      "offset": 32
    },
    {
      "sequence": 372,
      "elapsed_ms": 13111,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "245188",
      "offset": 0
    },
    {
      "sequence": 375,
      "elapsed_ms": 22305,
      "tool": "query",
      "input": "{\"capability\":\"minecraft:recipes\",\"input\":{\"output\":\"minecraft:wooden_pickaxe\",\"limit\":8}}",
      "revision": null,
      "offset": null
    },
    {
      "sequence": 378,
      "elapsed_ms": 12843,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "245898",
      "offset": 0
    },
    {
      "sequence": 381,
      "elapsed_ms": 9941,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "246097",
      "offset": 0
    },
    {
      "sequence": 385,
      "elapsed_ms": 11330,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":32,\"limit\":14}",
      "revision": "246325",
      "offset": 32
    },
    {
      "sequence": 388,
      "elapsed_ms": 9727,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "246523",
      "offset": 0
    },
    {
      "sequence": 391,
      "elapsed_ms": 10361,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":32,\"limit\":14}",
      "revision": "246731",
      "offset": 32
    },
    {
      "sequence": 394,
      "elapsed_ms": 15497,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "247043",
      "offset": 0
    },
    {
      "sequence": 398,
      "elapsed_ms": 30985,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":32,\"limit\":14}",
      "revision": "247664",
      "offset": 32
    },
    {
      "sequence": 401,
      "elapsed_ms": 11840,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "247904",
      "offset": 0
    },
    {
      "sequence": 404,
      "elapsed_ms": 20961,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":32,\"limit\":14}",
      "revision": "248325",
      "offset": 32
    },
    {
      "sequence": 407,
      "elapsed_ms": 14360,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "248614",
      "offset": 0
    },
    {
      "sequence": 410,
      "elapsed_ms": 13330,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "248883",
      "offset": 0
    },
    {
      "sequence": 413,
      "elapsed_ms": 17165,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":0,\"limit\":32}",
      "revision": "249227",
      "offset": 0
    },
    {
      "sequence": 416,
      "elapsed_ms": 5891,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":32,\"limit\":14}",
      "revision": "249346",
      "offset": 32
    },
    {
      "sequence": 419,
      "elapsed_ms": 10328,
      "tool": "observe",
      "input": "{\"view\":\"menu\",\"offset\":32,\"limit\":14}",
      "revision": "249555",
      "offset": 32
    }
  ]
}
"###;

pub fn events() -> Result<Vec<Value>> {
    let fixture: Value = serde_json::from_str(CAPTURE)?;
    let mut events = Vec::new();
    for step in fixture["steps"]
        .as_array()
        .ok_or("Fixture steps required")?
    {
        let sequence = step["sequence"]
            .as_u64()
            .ok_or("Fixture sequence required")?;
        events.push(json!({"event":"choice","sequence":sequence,
            "data":{"choice":{"tool":step["tool"],"input":step["input"]},
            "usage":{"elapsed_ms":step["elapsed_ms"]}}}));
        let result = if step["tool"] == "observe" {
            let mut page = fixture["pages"]
                .as_array()
                .ok_or("Fixture pages required")?
                .iter()
                .find(|page| page["page"]["offset"] == step["offset"])
                .ok_or("Recorded page missing")?
                .clone();
            page["revision"] = step["revision"].clone();
            page
        } else {
            json!({"view":"capability_query","scope":"Only query input and timing retained in this minimal menu fixture"})
        };
        events.push(json!({"event":"tool_result","sequence":sequence+1,
            "data":{"tool":step["tool"],"result":result}}));
    }
    Ok(events)
}
