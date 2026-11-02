package dev.minecraftsdk;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.ResourceLocation;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.tags.TagKey;
import net.minecraft.world.level.ClipContext;
import net.minecraft.world.phys.HitResult;
import net.minecraft.world.phys.Vec3;
import java.util.*;

/** Bounded, read-only, loaded-block targeting queries. No remote code or chunk generation. */
final class NativeBlockSearch {
    static final String CAPABILITY="minecraft:block_search";
    static final int MAX_RADIUS=Integer.getInteger("minecraftsdk.searchRadius",32);
    private static final int PAGE_MILLIS=20;
    private record Offset(int x,int y,int z,int distance){}
    private static final List<Offset> offsets=new ArrayList<>();
    private static final Map<net.minecraft.network.Connection,Long> last=new WeakHashMap<>();
    static void install(SdkService service){
        if(MAX_RADIUS<1||MAX_RADIUS>64)throw new IllegalArgumentException("SDK search radius requires 1..64");
        if(offsets.isEmpty()){
            for(int x=-MAX_RADIUS;x<=MAX_RADIUS;x++)for(int y=-MAX_RADIUS;y<=MAX_RADIUS;y++)for(int z=-MAX_RADIUS;z<=MAX_RADIUS;z++){
                int d=x*x+y*y+z*z;if(d<=MAX_RADIUS*MAX_RADIUS)offsets.add(new Offset(x,y,z,d));
            }
            offsets.sort(Comparator.comparingInt(Offset::distance).thenComparingInt(Offset::x).thenComparingInt(Offset::y).thenComparingInt(Offset::z));
        }
        String schema="""
            {"type":"object","properties":{"block":{"type":"string","maxLength":256},"tag":{"type":"string","maxLength":256},"radius":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1,"maximum":32},"offset":{"type":"integer","minimum":0}},"additionalProperties":false,"oneOf":[{"required":["block"]},{"required":["tag"]}]}
            """;
        var descriptor=new LinkedHashMap<String,Object>();descriptor.put("input_schema",JsonParser.parseString(schema));
        descriptor.put("description","Search runtime block IDs or tags around this character. Only loaded, non-air blocks hit by an unobstructed targeting ray are returned. Follow next_offset until complete; retry_after_ms is a deployment pace limit. Origin may change if the character moves.");
        descriptor.put("max_radius",MAX_RADIUS);descriptor.put("page_budget_ms",PAGE_MILLIS);descriptor.put("min_interval_ms",1000);
        service.registerObservationProvider(CAPABILITY,descriptor,NativeBlockSearch::query);
    }
    private static int number(JsonObject input,String key,int fallback,int min,int max){
        if(!input.has(key))return fallback;
        var value=input.get(key);if(!value.isJsonPrimitive()||!value.getAsJsonPrimitive().isNumber())throw new IllegalArgumentException("Integer query field required");
        int n=value.getAsBigDecimal().intValueExact();if(n<min||n>max)throw new IllegalArgumentException("Query exceeds deployment bounds");return n;
    }
    private static Object query(ServerPlayer player,JsonObject input){
        if(!Set.of("block","tag","radius","limit","offset").containsAll(input.keySet())||input.has("block")==input.has("tag"))throw new IllegalArgumentException("Exactly one runtime block or tag selector required");
        var selectorValue=input.get(input.has("block")?"block":"tag");
        if(!selectorValue.isJsonPrimitive()||!selectorValue.getAsJsonPrimitive().isString())throw new IllegalArgumentException("Registry selector must be a string");
        String selector=selectorValue.getAsString();
        if(selector.length()>256)throw new IllegalArgumentException("Registry selector exceeds bound");
        ResourceLocation id=ResourceLocation.parse(selector);
        var tag=input.has("tag")?TagKey.create(Registries.BLOCK,id):null;
        if(tag!=null&&BuiltInRegistries.BLOCK.getTag(tag).isEmpty()||tag==null&&!BuiltInRegistries.BLOCK.containsKey(id))throw new IllegalArgumentException("Unknown runtime selector");
        var selected=tag==null?BuiltInRegistries.BLOCK.get(id):null;
        int radius=number(input,"radius",MAX_RADIUS,1,MAX_RADIUS),limit=number(input,"limit",8,1,32),at=number(input,"offset",0,0,offsets.size());
        var origin=player.blockPosition();var level=player.serverLevel();var rows=new ArrayList<Map<String,Object>>();
        var answer=new LinkedHashMap<String,Object>();answer.put("origin",List.of(origin.getX(),origin.getY(),origin.getZ()));answer.put("radius",radius);answer.put("matches",rows);
        long now=System.nanoTime();Long previous=last.get(player.connection.getConnection());
        if(previous!=null&&now-previous<1_000_000_000L){answer.put("complete",false);answer.put("next_offset",at);answer.put("retry_after_ms",(1_000_000_000L-(now-previous))/1_000_000L+1);return answer;}
        last.put(player.connection.getConnection(),now);
        var eye=player.getEyePosition();int checked=0;
        for(;at<offsets.size();at++){
            var offset=offsets.get(at);if(offset.distance()>radius*radius)break;
            if(checked>0&&(checked%64)==0&&System.nanoTime()-now>=PAGE_MILLIS*1_000_000L)break;
            checked++;BlockPos p=origin.offset(offset.x(),offset.y(),offset.z());
            if(p.getY()<level.getMinBuildHeight()||p.getY()>=level.getMaxBuildHeight()||!level.hasChunkAt(p))continue;
            var state=level.getBlockState(p);if(state.isAir()||!(tag==null?state.is(selected):state.is(tag)))continue;
            var hit=level.clip(new ClipContext(eye,Vec3.atCenterOf(p),ClipContext.Block.OUTLINE,ClipContext.Fluid.NONE,player));
            if(hit.getType()!=HitResult.Type.BLOCK||!hit.getBlockPos().equals(p))continue;
            rows.add(Map.of("position",List.of(p.getX(),p.getY(),p.getZ()),"block",BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString(),"item",BuiltInRegistries.ITEM.getKey(state.getBlock().asItem()).toString(),"visible",true,"may_interact",level.mayInteract(player,p)));
            if(rows.size()>=limit){at++;break;}
        }
        boolean complete=at>=offsets.size()||offsets.get(at).distance()>radius*radius;
        answer.put("checked_blocks",checked);answer.put("complete",complete);answer.put("next_offset",complete?null:at);return answer;
    }
    private NativeBlockSearch(){}
}
