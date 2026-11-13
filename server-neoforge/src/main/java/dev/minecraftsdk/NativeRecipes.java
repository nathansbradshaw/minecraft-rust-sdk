package dev.minecraftsdk;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.resources.ResourceLocation;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.item.crafting.RecipeHolder;
import net.minecraft.world.item.crafting.ShapedRecipe;
import java.util.*;

/** Installed runtime knowledge only. Does not unlock recipes, fill menus or grant items. */
final class NativeRecipes {
    static final String CAPABILITY="minecraft:recipes";
    private static final int PAGE_BYTES=32768;
    private static final Map<net.minecraft.network.Connection,Long> last=new WeakHashMap<>();
    static void install(SdkService service){
        var descriptor=new LinkedHashMap<String,Object>();
        descriptor.put("input_schema",JsonParser.parseString("""
            {"type":"object","properties":{"recipe":{"type":"string","maxLength":256},"output":{"type":"string","maxLength":256},"type":{"type":"string","maxLength":256},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":32}},"additionalProperties":false}
            """));
        descriptor.put("description","Read installed recipe IDs, representative ingredients, results and this character's recipe-book knowledge. Select recipe, output item or type by exact runtime ID; filters combine. Follow next_offset. Pages are live, not atomic across recipe reloads. Dynamic/custom ingredient semantics may require a mod adapter. This query never unlocks recipes or proves craftability, crafting or pickup.");
        descriptor.put("page_budget_ms",20);descriptor.put("max_page_bytes",PAGE_BYTES);descriptor.put("scan_min_interval_ms",1000);
        service.registerObservationProvider(CAPABILITY,descriptor,NativeRecipes::query);
    }
    private static ResourceLocation selector(JsonObject input,String key){
        if(!input.has(key))return null;
        var value=input.get(key);
        if(!value.isJsonPrimitive()||!value.getAsJsonPrimitive().isString()||value.getAsString().length()>256||!value.getAsString().contains(":"))throw new IllegalArgumentException("Exact runtime identifier required");
        return ResourceLocation.parse(value.getAsString());
    }
    private static int number(JsonObject input,String key,int fallback,int max){
        if(!input.has(key))return fallback;
        var value=input.get(key);if(!value.isJsonPrimitive()||!value.getAsJsonPrimitive().isNumber())throw new IllegalArgumentException("Integer query field required");
        int n=value.getAsBigDecimal().intValueExact();if(n<(key.equals("limit")?1:0)||n>max)throw new IllegalArgumentException("Query exceeds deployment bounds");return n;
    }
    private static Map<String,Object> stack(ItemStack stack){
        return Map.of("item",BuiltInRegistries.ITEM.getKey(stack.getItem()).toString(),"count",stack.getCount());
    }
    private static Map<String,Object> describe(ServerPlayer player,RecipeHolder<?> holder){
        var recipe=holder.value();var row=new LinkedHashMap<String,Object>();
        row.put("id",holder.id().toString());row.put("type",BuiltInRegistries.RECIPE_TYPE.getKey(recipe.getType()).toString());
        row.put("serializer",BuiltInRegistries.RECIPE_SERIALIZER.getKey(recipe.getSerializer()).toString());
        row.put("known",player.getRecipeBook().contains(holder));row.put("special",recipe.isSpecial());
        var result=recipe.getResultItem(player.registryAccess());row.put("result_known",!result.isEmpty());row.put("result",stack(result));
        if(recipe instanceof ShapedRecipe shaped)row.put("shape",Map.of("width",shaped.getWidth(),"height",shaped.getHeight()));
        var ingredients=new ArrayList<Map<String,Object>>();var source=recipe.getIngredients();
        for(var ingredient:source.stream().limit(64).toList()){
            var options=ingredient.getItems();var samples=Arrays.stream(options).limit(16).map(NativeRecipes::stack).toList();
            ingredients.add(Map.of("empty",ingredient.isEmpty(),"matching_stacks",samples,"omitted_alternatives",Math.max(0,options.length-samples.size())));
        }
        row.put("ingredients",ingredients);row.put("omitted_ingredient_slots",Math.max(0,source.size()-ingredients.size()));
        row.put("ingredient_semantics","empty is the native explicit empty-slot flag, not an inference from matching_stacks. Runtime representative matching stacks do not certify custom quantities, components or dynamic results; these may require a namespaced mod adapter. An empty matching_stacks list alone establishes neither an empty slot nor an impossible recipe.");
        if(SdkService.JSON.toJson(row).length()>8192){row.remove("ingredients");row.put("ingredients_omitted_for_budget",true);}
        return row;
    }
    private static Object query(ServerPlayer player,JsonObject input){
        if(!Set.of("recipe","output","type","offset","limit").containsAll(input.keySet()))throw new IllegalArgumentException("Unknown recipe query field");
        var id=selector(input,"recipe");var output=selector(input,"output");var type=selector(input,"type");
        int at=number(input,"offset",0,Integer.MAX_VALUE),limit=number(input,"limit",8,32),bytes=0,checked=0,dynamic=0;
        var rows=new ArrayList<Map<String,Object>>();long started=System.nanoTime();
        if(id==null){
            Long previous=last.get(player.connection.getConnection());
            if(previous!=null&&started-previous<1_000_000_000L)return Map.of("format","minecraft-runtime-recipes-v1","matches",rows,"complete",false,"next_offset",at,"retry_after_ms",(1_000_000_000L-(started-previous))/1_000_000L+1);
            last.put(player.connection.getConnection(),started);
        }
        var manager=player.serverLevel().getRecipeManager();
        var all=id==null?manager.getRecipes().stream().sorted(Comparator.comparing(h->h.id().toString())).toList():manager.byKey(id).stream().toList();
        if(at>all.size())throw new IllegalArgumentException("Recipe offset exceeds current snapshot; restart at zero");
        for(;at<all.size();at++){
            if(checked>0&&System.nanoTime()-started>=20_000_000L)break;
            var holder=all.get(at);var recipe=holder.value();checked++;
            if(type!=null&&!type.equals(BuiltInRegistries.RECIPE_TYPE.getKey(recipe.getType())))continue;
            var result=recipe.getResultItem(player.registryAccess());
            if(output!=null){if(result.isEmpty()){dynamic++;continue;}if(!output.equals(BuiltInRegistries.ITEM.getKey(result.getItem())))continue;}
            var row=describe(player,holder);int size=SdkService.JSON.toJson(row).getBytes(java.nio.charset.StandardCharsets.UTF_8).length;
            if(bytes+size>PAGE_BYTES)break;
            rows.add(row);bytes+=size;if(rows.size()>=limit){at++;break;}
        }
        var answer=new LinkedHashMap<String,Object>();answer.put("format","minecraft-runtime-recipes-v1");answer.put("matches",rows);
        answer.put("complete",at==all.size());answer.put("next_offset",at==all.size()?null:at);answer.put("checked_recipes",checked);
        answer.put("dynamic_results_not_matched",dynamic);answer.put("unknown_recipe",id!=null&&all.isEmpty());
        answer.put("binding","Installed runtime recipe knowledge only. known describes this character's book, not ingredients or menu compatibility. Output-filter misses cannot exclude dynamic/custom recipes. Pages may change after reload; recheck an exact ID before use.");
        return answer;
    }
    private NativeRecipes(){}
}
