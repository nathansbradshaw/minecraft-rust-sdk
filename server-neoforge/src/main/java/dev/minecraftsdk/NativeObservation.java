package dev.minecraftsdk;

import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.item.ItemStack;
import java.util.*;

/** Bounded observations, always collected together on the authoritative server thread. */
final class NativeObservation {
    private static Map<String,Object> stack(ItemStack stack,ServerPlayer player){
        var result=new LinkedHashMap<String,Object>();result.put("item",BuiltInRegistries.ITEM.getKey(stack.getItem()).toString());result.put("count",stack.getCount());
        result.put("components",stack.isEmpty()?"":stack.save(player.registryAccess()).toString());return result;
    }
    static Object snapshot(MinecraftServer server,ServerPlayer player){
        if(!server.isSameThread())throw new IllegalStateException("Main server thread required");
        var world=player.serverLevel();var origin=player.blockPosition();var menu=player.containerMenu;
        var inventory=new ArrayList<Map<String,Object>>();
        for(int i=0;i<player.getInventory().getContainerSize();i++){var row=new LinkedHashMap<>(stack(player.getInventory().getItem(i),player));row.put("slot",i);inventory.add(row);}
        var slots=new ArrayList<Map<String,Object>>();
        if(menu.slots.size()>512)throw new IllegalStateException("Menu observation capability unavailable for more than 512 slots");
        for(var slot:menu.slots){var row=new LinkedHashMap<>(stack(slot.getItem(),player));row.put("slot",slot.index);row.put("inventory_slot",slot.getContainerSlot());row.put("player_inventory",slot.container==player.getInventory());row.put("may_pickup",slot.mayPickup(player));row.put("may_place_carried",slot.mayPlace(menu.getCarried()));slots.add(row);}
        var blocks=new ArrayList<Map<String,Object>>();var unknown=new ArrayList<List<Integer>>();
        for(int x=-4;x<=4;x++)for(int z=-4;z<=4;z++){
            if(!world.hasChunkAt(origin.offset(x,0,z))){unknown.add(List.of(origin.getX()+x,origin.getZ()+z));continue;}
            for(int y=-4;y<=4;y++){
                var pos=origin.offset(x,y,z);var state=world.getBlockState(pos);var row=new LinkedHashMap<String,Object>();
                row.put("position",List.of(pos.getX(),pos.getY(),pos.getZ()));row.put("block",BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());row.put("state",state.toString());row.put("replaceable",state.canBeReplaced());row.put("may_interact",world.mayInteract(player,pos));
                float progress=state.getDestroyProgress(player,world,pos);row.put("destroy_progress",progress>0?Math.min(1.0f,progress):0f);
                row.put("collision",state.getCollisionShape(world,pos).toAabbs().stream().map(a->List.of(a.minX,a.minY,a.minZ,a.maxX,a.maxY,a.maxZ)).toList());blocks.add(row);
            }
        }
        var entities=new ArrayList<Map<String,Object>>();
        for(var entity:world.getEntities(player,player.getBoundingBox().inflate(12))){if(entities.size()>=128)break;var row=new LinkedHashMap<String,Object>();row.put("entity_id",entity.getId());row.put("uuid",entity.getUUID().toString());row.put("type",BuiltInRegistries.ENTITY_TYPE.getKey(entity.getType()).toString());row.put("position",List.of(entity.getX(),entity.getY(),entity.getZ()));row.put("visible",player.hasLineOfSight(entity));entities.add(row);}
        var own=new LinkedHashMap<String,Object>();own.put("uuid",player.getUUID().toString());own.put("name",player.getGameProfile().getName());own.put("entity_id",player.getId());own.put("dimension",world.dimension().location().toString());own.put("position",List.of(player.getX(),player.getY(),player.getZ()));own.put("yaw",player.getYRot());own.put("pitch",player.getXRot());own.put("on_ground",player.onGround());own.put("sprinting",player.isSprinting());own.put("sneaking",player.isShiftKeyDown());own.put("health",player.getHealth());own.put("food",player.getFoodData().getFoodLevel());own.put("inventory",inventory);own.put("selected",player.getInventory().selected);
        var mining=(dev.minecraftsdk.mixin.GameModeAccessor)player.gameMode;var target=mining.sdk$miningPosition();own.put("mining",Map.of("active",mining.sdk$mining(),"block",List.of(target.getX(),target.getY(),target.getZ())));own.put("accepted_chat",NativeChat.snapshot(player));
        return Map.of("revision",Integer.toString(server.getTickCount()),"player",own,"world",Map.of("game_time",world.getGameTime(),"day_time",world.getDayTime(),"blocks",blocks,"entities",entities,"unknown_columns",unknown),"menu",Map.of("menu_id",menu.containerId,"state_id",menu.getStateId(),"type",menu==player.inventoryMenu?"minecraft:inventory":BuiltInRegistries.MENU.getKey(menu.getType()).toString(),"slots",slots,"carried",stack(menu.getCarried(),player)));
    }
    private NativeObservation(){}
}
