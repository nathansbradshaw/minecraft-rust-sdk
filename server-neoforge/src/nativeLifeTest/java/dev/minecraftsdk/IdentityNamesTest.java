package dev.minecraftsdk;

/** Admission regression: readable names cannot turn into arbitrary profile renames. */
public final class IdentityNamesTest {
    private static void check(boolean value,String reason){if(!value)throw new AssertionError(reason);}
    public static void main(String[] args){
        String old=".S_d397045b67f5",name=".S_MayaWoods";
        for(String valid:new String[]{old,name,".AI_Woods",".S_OscarGreen9"})check(IdentityNames.valid(valid),valid);
        for(String invalid:new String[]{"MayaWoods",".S_",".S_bad name",".S_../Maya",".S_MayaWoods123456", ".AI_"})check(!IdentityNames.valid(invalid),invalid);
        check(!IdentityNames.valid(null),"null name refused");
        check(IdentityNames.canUpgrade(old,name,false,false),"offline generated identity can upgrade");
        check(!IdentityNames.canUpgrade(old,name,true,false),"connected identity is pinned");
        check(!IdentityNames.canUpgrade(old,name,false,true),"outstanding ticket is pinned");
        check(!IdentityNames.canUpgrade(name,".S_OwenStone",false,false),"readable name is permanent");
        check(!IdentityNames.canUpgrade(".AI_Woods",name,false,false),"legacy character remains intact");
        check(!IdentityNames.canUpgrade(old,".AI_Woods",false,false),"upgrade cannot claim legacy namespace");
        check(!IdentityNames.canUpgrade(old,".S_123456abcdef",false,false),"upgrade cannot select another hash name");
    }
}
